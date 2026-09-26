// SPDX-License-Identifier: MIT OR Apache-2.0

//! JavaScript-initiated view, transfer, detach, and resize operations on
//! Rust-owned external `ArrayBuffer`s.
//!
//! Construction queries the backing-store pointer, which pins the buffer in
//! `JavaScriptCore`. These tests pin the resulting contract: JavaScript cannot
//! detach or resize an external buffer, every `transfer()` form copies into a
//! new engine-owned buffer, Rust records no allocation for that copy, and the
//! Rust deallocator runs once, on the runtime thread, only after the original
//! buffer is unreachable or the runtime is torn down.
//!
//! Engine-owned buffers are used as controls: the same operations do detach
//! them, so a still-attached external source is not a missing engine feature.

use super::{Context, ExternalBuffer, Runtime, RuntimeError};
use rustjsi_backend::{
    BackendBase, BackendScope, CallReceiver, OwnedExternalBufferScope, ValueKind,
};

const BYTE_LEN: usize = 4096;
const GC_ROUNDS: usize = 32;
const REACHABLE_GC_ROUNDS: usize = 8;

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

fn assert_transfer_supported(cx: &mut Context<'_>) {
    assert!(
        js_bool(
            cx,
            "typeof ArrayBuffer.prototype.transfer === 'function' && \
             typeof ArrayBuffer.prototype.transferToFixedLength === 'function' && \
             Object.getOwnPropertyDescriptor(ArrayBuffer.prototype, 'detached') !== undefined",
        ),
        "this JavaScriptCore lacks ArrayBuffer transfer; the transfer path is untested"
    );
}

/// Asserts that the Rust ledger holds only the original external allocation:
/// a JavaScript transfer must not have been routed through Rust storage.
fn assert_ledger_holds_only_origin(cx: &Context<'_>) {
    assert_eq!(cx.shared.external_buffers.live_allocations(), 1);
    assert_eq!(cx.shared.external_buffers.live_bytes(), BYTE_LEN);
    assert_eq!(cx.shared.external_buffers.deallocations(), 0);
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

/// Collects repeatedly while `buffer` is still reachable from JavaScript and
/// requires that the engine never releases it.
fn collect_while_reachable(runtime: &mut Runtime, buffer: &ExternalBuffer) {
    for _ in 0..REACHABLE_GC_ROUNDS {
        force_collection(runtime);
        assert!(
            !buffer.is_deallocated(),
            "reachable external allocation was released"
        );
    }
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

/// Asserts one release of the original allocation, through the original
/// pointer, on the thread that owns the runtime.
///
/// Every collection and teardown in these tests is driven synchronously from
/// the runtime thread. The deallocator itself is safe on any thread; that case
/// is covered by the synthetic off-thread release test.
fn assert_released_exactly_once(runtime: &Runtime, buffer: &ExternalBuffer) {
    assert!(buffer.is_deallocated());
    assert_eq!(buffer.deallocator_received_origin(), Some(true));
    assert_eq!(buffer.deallocator_ran_on_runtime_thread(), Some(true));
    assert_eq!(runtime.shared.external_buffers.deallocations(), 1);
    assert_eq!(runtime.shared.external_buffers.live_allocations(), 0);
    assert_eq!(runtime.shared.external_buffers.live_bytes(), 0);
}

/// Asserts that `result` is a copy of the first `result_len` bytes of the
/// external `source`, that `source` stayed attached at full length, and that
/// the two backings are independent in both directions.
fn assert_copied_transfer(cx: &mut Context<'_>, source: &str, result: &str, result_len: usize) {
    assert!(
        !js_bool(cx, &format!("{source}.detached")),
        "JavaScript detached a pinned external buffer"
    );
    assert_js_integer(cx, &format!("{source}.byteLength"), BYTE_LEN);
    assert!(js_bool(
        cx,
        &format!("matchesPattern(new Uint8Array({source}), 0)")
    ));
    assert_js_integer(cx, &format!("{result}.byteLength"), result_len);
    let preserved = result_len.min(BYTE_LEN);
    assert!(js_bool(
        cx,
        &format!(
            "matchesPattern(new Uint8Array({result}, 0, {preserved}), 0)\
             && new Uint8Array({result}, {preserved}).every((b) => b === 0)"
        )
    ));
    if preserved != 0 {
        assert!(js_bool(
            cx,
            &format!(
                "(() => {{ const s = new Uint8Array({source}); const r = new Uint8Array({result});\
                 r[0] = s[0] ^ 0xff; const sourceKept = s[0] === pattern(0);\
                 r[0] = pattern(0); s[0] = pattern(0) ^ 0xff; const resultKept = r[0] === pattern(0);\
                 s[0] = pattern(0); return sourceKept && resultKept; }})()"
            )
        ));
    }
}

#[test]
fn javascript_transfer_never_frees_external_bytes_while_reachable() {
    let mut runtime = Runtime::new().unwrap();
    let buffer = runtime
        .with_context(|cx| cx.install_external_buffer("external", payload()).unwrap())
        .unwrap();
    assert_eq!(buffer.byte_len(), BYTE_LEN);
    // The Rust-to-JavaScript route copies nothing: JSC retained the origin.
    assert_eq!(buffer.backing_store_matches_origin(), Some(true));

    runtime
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

            assert_transfer_supported(cx);
            // Control: an engine-owned buffer is detached by transfer, and its
            // views then read nothing and reject access.
            assert!(js_bool(
                cx,
                "(() => { const own = new ArrayBuffer(8); const view = new Uint8Array(own);\
                 const data = new DataView(own); const next = own.transfer();\
                 let rejected = false; try { data.getUint8(0); } catch (e) { rejected = e instanceof TypeError; }\
                 return own.detached && own.byteLength === 0 && view.length === 0\
                 && rejected && next.byteLength === 8; })()",
            ));

            cx.eval(
                "globalThis.moved = external.transfer()",
                "external-transfer.js",
            )
            .unwrap();
            assert_copied_transfer(cx, "external", "moved", BYTE_LEN);
            // The source keeps its backing, so its views stay valid, and a
            // second transfer is another copy rather than a detach.
            assert!(js_bool(
                cx,
                "matchesPattern(offsetView, 16) && offsetView.byteOffset === 16\
                 && dataView.getUint8(0) === pattern(8)",
            ));
            cx.eval(
                "globalThis.second = external.transferToFixedLength()",
                "external-transfer-again.js",
            )
            .unwrap();
            assert_copied_transfer(cx, "external", "second", BYTE_LEN);
            // Both copies live in engine storage; Rust allocated nothing.
            assert_ledger_holds_only_origin(cx);
        })
        .unwrap();

    // The original buffer, its views, and both copies stay reachable.
    collect_while_reachable(&mut runtime, &buffer);
    runtime
        .with_context(|cx| {
            assert!(js_bool(
                cx,
                "matchesPattern(new Uint8Array(external), 0) && matchesPattern(offsetView, 16)",
            ));
            cx.eval(
                "delete globalThis.external; delete globalThis.offsetView;\
                 delete globalThis.dataView",
                "external-release-source.js",
            )
            .unwrap();
        })
        .unwrap();

    // The copies do not share the original allocation, so it can be freed now.
    collect_until_deallocated(&mut runtime, &buffer);
    assert_released_exactly_once(&runtime, &buffer);

    // A copy that aliased the freed allocation would read freed memory here.
    // The system engine is not sanitizer instrumented, so this content check
    // is the direct evidence; AddressSanitizer only covers the Rust side.
    runtime
        .with_context(|cx| {
            assert!(js_bool(
                cx,
                "matchesPattern(new Uint8Array(moved), 0) && matchesPattern(new Uint8Array(second), 0)",
            ));
            cx.eval(
                "delete globalThis.moved; delete globalThis.second",
                "external-release-result.js",
            )
            .unwrap();
        })
        .unwrap();
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
            assert_transfer_supported(cx);
            cx.eval(
                "globalThis.view = new Uint8Array(external, 1);\
                 globalThis.moved = external.transfer();\
                 globalThis.again = moved.transferToFixedLength()",
                "external-transfer-chain.js",
            )
            .unwrap();
            // The external source is pinned; its engine-owned copy is not, so
            // transferring the copy detaches it.
            assert_copied_transfer(cx, "external", "again", BYTE_LEN);
            assert!(js_bool(
                cx,
                &format!(
                    "moved.detached && moved.byteLength === 0 && view.length === {}",
                    BYTE_LEN - 1
                ),
            ));
            assert!(js_bool(cx, "matchesPattern(view, 1)"));
            assert_ledger_holds_only_origin(cx);
        })
        .unwrap();

    collect_while_reachable(&mut runtime, &buffer);

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
            assert_transfer_supported(cx);
            assert!(js_bool(
                cx,
                &format!("external.resizable === false && external.maxByteLength === {BYTE_LEN}"),
            ));
            assert!(js_throws_type_error(cx, "external.resize(1)"));
            assert_js_integer(cx, "external.byteLength", BYTE_LEN);

            // Length-changing transfers copy the preserved prefix, zero-fill
            // any extension, and leave the external source attached.
            cx.eval(
                &format!(
                    "globalThis.shorter = external.transferToFixedLength(16);\
                     globalThis.empty = external.transfer(0);\
                     globalThis.longer = external.transfer({})",
                    2 * BYTE_LEN
                ),
                "external-transfer-length.js",
            )
            .unwrap();
            assert_copied_transfer(cx, "external", "shorter", 16);
            assert_copied_transfer(cx, "external", "empty", 0);
            assert_copied_transfer(cx, "external", "longer", 2 * BYTE_LEN);
            assert_ledger_holds_only_origin(cx);

            // Control: the engine-owned copy is detached by a length change.
            cx.eval(
                &format!("globalThis.grown = shorter.transfer({})", 2 * BYTE_LEN),
                "external-transfer-grow-copy.js",
            )
            .unwrap();
            assert!(js_bool(cx, "shorter.detached && shorter.byteLength === 0"));
            assert!(js_bool(
                cx,
                "matchesPattern(new Uint8Array(grown, 0, 16), 0)\
                 && new Uint8Array(grown, 16).every((b) => b === 0)",
            ));
        })
        .unwrap();
    collect_while_reachable(&mut runtime, &buffer);
    runtime
        .with_context(|cx| {
            cx.eval(
                "delete globalThis.external; delete globalThis.shorter;\
                 delete globalThis.empty; delete globalThis.longer; delete globalThis.grown",
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
                           globalThis.keptResult = moved;\
                           return !buffer.detached && buffer.byteLength === {BYTE_LEN}\
                             && matchesPattern(view, 8) && matchesPattern(new Uint8Array(moved), 0);\
                         }})"
                    ),
                    "common-external-transfer.js",
                )
                .unwrap();
            let ok = scope
                .call(transfer, CallReceiver::Global, &[external])
                .unwrap();
            assert_eq!(scope.as_boolean(ok), Ok(true));
            assert_eq!(scope.kind(external), Ok(ValueKind::Buffer));

            // Rust exposes no byte access to buffers, so the Rust-side
            // operation after a detach is classification. Check it on an
            // engine-owned buffer that JavaScript really detached.
            let detached = scope
                .evaluate(
                    "(() => { const own = new ArrayBuffer(8); own.transfer(); return own; })()",
                    "common-detached-buffer.js",
                )
                .unwrap();
            assert_eq!(scope.kind(detached), Ok(ValueKind::Buffer));
            let is_detached = scope
                .evaluate("(buffer) => buffer.detached", "common-is-detached.js")
                .unwrap();
            let is_detached = scope
                .call(is_detached, CallReceiver::Global, &[detached])
                .unwrap();
            assert_eq!(scope.as_boolean(is_detached), Ok(true));
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
