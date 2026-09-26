// SPDX-License-Identifier: MIT OR Apache-2.0

//! JavaScript-initiated view, transfer, detach, and resize operations on
//! Rust-owned external `ArrayBuffer`s.
//!
//! Rust keeps no pointer to transferred payload bytes, so the properties under
//! test are the engine-visible ones: what JavaScript observes on the original
//! buffer and its views, whether `transfer()` moved or copied the external
//! backing, and that the Rust deallocator runs exactly once, only after every
//! JavaScript buffer that can still reach the original allocation is gone.

use super::{Context, ExternalBuffer, Runtime, RuntimeError};
use rustjsi_backend::{
    BackendBase, BackendScope, CallReceiver, OwnedExternalBufferScope, ValueKind,
};

const BYTE_LEN: usize = 4096;
const GC_ROUNDS: usize = 32;

/// Byte `i` of every payload; mirrored by `pattern(i)` in JavaScript.
fn pattern_byte(index: usize) -> u8 {
    u8::try_from((index * 31 + 7) % 256).unwrap()
}

const JS_PATTERN: &str = "globalThis.pattern = (i) => (i * 31 + 7) % 256;\
     globalThis.matchesPattern = (bytes, offset) => {\
       for (let i = 0; i < bytes.length; i++) {\
         if (bytes[i] !== pattern(offset + i)) return false;\
       }\
       return true;\
     };";

fn payload() -> Box<[u8]> {
    (0..BYTE_LEN).map(pattern_byte).collect()
}

/// How the engine satisfied `ArrayBuffer.prototype.transfer()` for an
/// external buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TransferRoute {
    /// The source was detached and the result took over the external backing.
    Moved,
    /// The source stayed attached and the result received its own copy.
    Copied,
}

fn js_bool(cx: &mut Context<'_>, source: &str) -> bool {
    let value = cx.eval(source, "external-transfer.js").unwrap();
    cx.boolean(&value).unwrap()
}

/// Asserts a JavaScript integer expression without converting through `f64`.
fn assert_js_integer(cx: &mut Context<'_>, expression: &str, expected: usize) {
    assert!(
        js_bool(cx, &format!("({expression}) === {expected}")),
        "expected `{expression}` to be {expected}"
    );
}

fn js_string(cx: &mut Context<'_>, source: &str) -> String {
    let value = cx.eval(source, "external-transfer.js").unwrap();
    cx.string(&value).unwrap()
}

fn js_throws_type_error(cx: &mut Context<'_>, expression: &str) -> bool {
    js_bool(
        cx,
        &format!(
            "(() => {{ try {{ {expression}; return false; }} catch (e) {{ return e instanceof TypeError; }} }})()"
        ),
    )
}

fn transfer_supported(cx: &mut Context<'_>) -> bool {
    js_bool(
        cx,
        "typeof ArrayBuffer.prototype.transfer === 'function' && \
         typeof ArrayBuffer.prototype.transferToFixedLength === 'function' && \
         Object.getOwnPropertyDescriptor(ArrayBuffer.prototype, 'detached') !== undefined",
    )
}

fn force_collection(runtime: &mut Runtime) {
    runtime
        .with_context(|cx| {
            cx.collect_garbage().unwrap();
            cx.eval(
                "Array.from({ length: 4096 }, (_, i) => ({ i }))",
                "external-transfer-gc-churn.js",
            )
            .unwrap();
        })
        .unwrap();
}

fn collect_until_deallocated(runtime: &mut Runtime, buffer: &ExternalBuffer) {
    for _ in 0..GC_ROUNDS {
        if buffer.is_deallocated() {
            return;
        }
        force_collection(runtime);
    }
    panic!("external ArrayBuffer owner was not deallocated");
}

fn collect_until_ledger_empty(runtime: &mut Runtime) {
    for _ in 0..GC_ROUNDS {
        if runtime.shared.external_buffers.live_allocations() == 0 {
            return;
        }
        force_collection(runtime);
    }
    panic!(
        "expected an empty external ledger, observed {} allocations",
        runtime.shared.external_buffers.live_allocations()
    );
}

fn assert_released_exactly_once(runtime: &Runtime, buffer: &ExternalBuffer) {
    assert!(buffer.is_deallocated());
    assert_eq!(buffer.deallocator_received_origin(), Some(true));
    assert_eq!(runtime.shared.external_buffers.deallocations(), 1);
    assert_eq!(runtime.shared.external_buffers.live_allocations(), 0);
    assert_eq!(runtime.shared.external_buffers.live_bytes(), 0);
}

/// Classifies the transfer that JavaScript just performed from `source` into
/// `result`, checking that exactly one of the two coherent outcomes occurred.
fn classify_transfer(cx: &mut Context<'_>, source: &str, result: &str) -> TransferRoute {
    let detached = js_bool(cx, &format!("{source}.detached"));
    assert_js_integer(cx, &format!("{result}.byteLength"), BYTE_LEN);
    assert!(js_bool(
        cx,
        &format!("matchesPattern(new Uint8Array({result}), 0)")
    ));
    if detached {
        // A detached source must report length 0.
        assert_js_integer(cx, &format!("{source}.byteLength"), 0);
        TransferRoute::Moved
    } else {
        assert_js_integer(cx, &format!("{source}.byteLength"), BYTE_LEN);
        // A copy must be independent in both directions.
        assert!(js_bool(
            cx,
            &format!(
                "(() => {{ const s = new Uint8Array({source}); const r = new Uint8Array({result});\
                 r[0] = s[0] ^ 0xff; const first = s[0] === pattern(0);\
                 s[1] = r[1] ^ 0xff; const second = r[1] === pattern(1);\
                 r[0] = pattern(0); s[1] = pattern(1); return first && second; }})()"
            )
        ));
        TransferRoute::Copied
    }
}

#[test]
fn javascript_transfer_never_frees_external_bytes_while_reachable() {
    let mut runtime = Runtime::new().unwrap();
    let buffer = runtime
        .with_context(|cx| cx.install_external_buffer("external", payload()).unwrap())
        .unwrap();
    assert_eq!(buffer.byte_len(), BYTE_LEN);

    let route = runtime
        .with_context(|cx| {
            cx.eval(JS_PATTERN, "external-pattern.js").unwrap();
            // Offset views read the external bytes in place.
            assert!(js_bool(
                cx,
                "globalThis.offsetView = new Uint8Array(external, 16, 32);\
                 globalThis.dataView = new DataView(external, 8, 8);\
                 matchesPattern(offsetView, 16) && dataView.getUint8(0) === pattern(8)\
                 && offsetView.byteOffset === 16 && dataView.byteLength === 8",
            ));

            // Bare JSC global contexts do not expose the structured-clone
            // transfer route. If that changes, it needs coverage here too.
            assert_eq!(js_string(cx, "typeof structuredClone"), "undefined");

            assert!(
                transfer_supported(cx),
                "this JavaScriptCore lacks ArrayBuffer transfer; the transfer path is untested"
            );
            // Control: an engine-owned buffer is detached by transfer, so a
            // non-detached external source below is not a missing feature.
            assert!(js_bool(
                cx,
                "(() => { const own = new ArrayBuffer(8); const view = new Uint8Array(own);\
                 const next = own.transfer(); return own.detached && own.byteLength === 0\
                 && view.length === 0 && next.byteLength === 8; })()",
            ));

            cx.eval(
                "globalThis.moved = external.transfer()",
                "external-transfer.js",
            )
            .unwrap();
            classify_transfer(cx, "external", "moved")
        })
        .unwrap();
    eprintln!("external ArrayBuffer transfer route: {route:?}");

    runtime
        .with_context(|cx| match route {
            TransferRoute::Moved => {
                assert_js_integer(cx, "offsetView.length", 0);
                assert_js_integer(cx, "offsetView.byteOffset", 0);
                assert!(js_throws_type_error(cx, "dataView.getUint8(0)"));
                assert!(js_throws_type_error(cx, "new Uint8Array(external)"));
                assert!(js_throws_type_error(cx, "external.transfer()"));
                assert!(js_throws_type_error(cx, "external.slice(0)"));
            }
            TransferRoute::Copied => {
                // The source keeps its backing; its views remain valid.
                assert!(js_bool(
                    cx,
                    "matchesPattern(offsetView, 16) && dataView.getUint8(0) === pattern(8)",
                ));
            }
        })
        .unwrap();
    assert!(!buffer.is_deallocated());

    // Release the original buffer and its views while the transfer result stays live.
    runtime
        .with_context(|cx| {
            cx.eval(
                "delete globalThis.external; delete globalThis.offsetView;\
                 delete globalThis.dataView",
                "external-release-source.js",
            )
            .unwrap();
        })
        .unwrap();
    match route {
        TransferRoute::Moved => {
            // The result owns the original allocation: it must not be freed.
            for _ in 0..8 {
                force_collection(&mut runtime);
                assert!(!buffer.is_deallocated());
            }
        }
        TransferRoute::Copied => {
            // The result owns a copy, so the original may be freed now.
            collect_until_deallocated(&mut runtime, &buffer);
            assert_released_exactly_once(&runtime, &buffer);
        }
    }

    // The transfer result stays readable in either route. In the copied route
    // the original allocation is already freed here, so a result that aliased
    // it would read freed memory. The system engine is not sanitizer
    // instrumented, so this content check is the direct evidence; AddressSanitizer
    // only covers the Rust side of the release.
    runtime
        .with_context(|cx| {
            assert!(js_bool(cx, "matchesPattern(new Uint8Array(moved), 0)"));
            cx.eval("delete globalThis.moved", "external-release-result.js")
                .unwrap();
        })
        .unwrap();
    collect_until_deallocated(&mut runtime, &buffer);
    for _ in 0..4 {
        force_collection(&mut runtime);
    }
    assert_released_exactly_once(&runtime, &buffer);
    runtime.invalidate().unwrap();
    assert_released_exactly_once(&runtime, &buffer);
}

#[test]
fn transferred_external_buffers_release_once_at_runtime_invalidation() {
    let mut runtime = Runtime::new().unwrap();
    let buffer = runtime
        .with_context(|cx| cx.install_external_buffer("external", payload()).unwrap())
        .unwrap();
    runtime
        .with_context(|cx| {
            cx.eval(JS_PATTERN, "external-pattern.js").unwrap();
            assert!(transfer_supported(cx));
            cx.eval(
                "globalThis.view = new Uint8Array(external, 1);\
                 globalThis.moved = external.transfer();\
                 globalThis.again = moved.transferToFixedLength()",
                "external-transfer-chain.js",
            )
            .unwrap();
            assert!(js_bool(cx, "matchesPattern(new Uint8Array(again), 0)"));
        })
        .unwrap();
    force_collection(&mut runtime);

    // Every JavaScript buffer is still reachable when the runtime is torn down.
    runtime.invalidate().unwrap();
    assert_released_exactly_once(&runtime, &buffer);
    assert!(matches!(
        runtime.with_context(|_| ()),
        Err(RuntimeError::Invalidated)
    ));
    assert_released_exactly_once(&runtime, &buffer);
}

#[test]
fn external_buffers_reject_resize_and_copy_on_length_change() {
    let mut runtime = Runtime::new().unwrap();
    let buffer = runtime
        .with_context(|cx| cx.install_external_buffer("external", payload()).unwrap())
        .unwrap();
    runtime
        .with_context(|cx| {
            cx.eval(JS_PATTERN, "external-pattern.js").unwrap();
            assert!(transfer_supported(cx));
            assert!(js_bool(
                cx,
                &format!("external.resizable === false && external.maxByteLength === {BYTE_LEN}"),
            ));
            assert!(js_throws_type_error(cx, "external.resize(1)"));
            assert_js_integer(cx, "external.byteLength", BYTE_LEN);

            // Length-changing transfers must produce a buffer of the requested
            // length with the preserved prefix and zero-filled extension.
            cx.eval(
                &format!(
                    "globalThis.shorter = external.transferToFixedLength(16);\
                     globalThis.longer = shorter.transfer({})",
                    2 * BYTE_LEN
                ),
                "external-transfer-length.js",
            )
            .unwrap();
            assert_js_integer(cx, "shorter.byteLength", 0);
            assert!(js_bool(
                cx,
                &format!(
                    "longer.byteLength === {} && matchesPattern(new Uint8Array(longer, 0, 16), 0)\
                     && new Uint8Array(longer, 16).every((b) => b === 0)",
                    2 * BYTE_LEN
                ),
            ));
            let source_detached = js_bool(cx, "external.detached");
            eprintln!("external source detached by length-changing transfer: {source_detached}");
            if !source_detached {
                assert!(js_bool(cx, "matchesPattern(new Uint8Array(external), 0)"));
            }
            cx.eval(
                "delete globalThis.external; delete globalThis.shorter;\
                  delete globalThis.longer",
                "external-release-lengths.js",
            )
            .unwrap();
        })
        .unwrap();
    collect_until_deallocated(&mut runtime, &buffer);
    assert_released_exactly_once(&runtime, &buffer);
}

#[test]
fn common_scope_external_buffer_survives_javascript_transfer() {
    let mut runtime = Runtime::new().unwrap();
    runtime
        .with_backend(|backend| {
            let scope = backend.open_scope().unwrap();
            let external = scope.externalize(payload()).unwrap();
            let transfer = scope
                .evaluate(
                    &format!(
                        "{JS_PATTERN} (function (buffer) {{\
                           const view = new Uint8Array(buffer, 8);\
                           const moved = buffer.transfer();\
                           const ok = matchesPattern(new Uint8Array(moved), 0) &&\
                             (buffer.detached ? view.length === 0 && buffer.byteLength === 0\
                               : matchesPattern(view, 8));\
                           globalThis.keptResult = moved;\
                           return ok;\
                         }})"
                    ),
                    "common-external-transfer.js",
                )
                .unwrap();
            let ok = scope
                .call(transfer, CallReceiver::Global, &[external])
                .unwrap();
            assert_eq!(scope.as_boolean(ok), Ok(true));
            // Classification needs no payload access, even after a possible detach.
            assert_eq!(scope.kind(external), Ok(ValueKind::Buffer));
        })
        .unwrap();
    runtime
        .with_context(|cx| {
            cx.eval("delete globalThis.keptResult", "common-release.js")
                .unwrap();
        })
        .unwrap();
    collect_until_ledger_empty(&mut runtime);
    assert_eq!(runtime.shared.external_buffers.deallocations(), 1);
    assert_eq!(runtime.shared.external_buffers.live_bytes(), 0);
}
