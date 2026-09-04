// SPDX-License-Identifier: MIT OR Apache-2.0

use super::{HostError, JsError, JsException, Runtime};
use rustjsi_backend::{BackendBase, BackendError, BackendScope, ValueKind};

fn thrown(runtime: &mut Runtime, source: &str) -> JsException {
    runtime
        .with_context(|cx| {
            let JsError::Exception(error) = cx.eval(source, "throw.js").unwrap_err() else {
                panic!("expected JavaScript exception");
            };
            error
        })
        .unwrap()
}

#[test]
fn large_exception_has_a_bounded_rust_message() {
    let mut runtime = Runtime::new().unwrap();
    runtime
        .with_context(|cx| {
            let error = cx
                .eval("throw 'x'.repeat(512 * 1024)", "large.js")
                .unwrap_err();
            let JsError::Exception(error) = error else {
                panic!("expected JavaScript exception");
            };
            assert!(error.message().len() <= 4096);
            assert!(error.message.capacity() <= 4097);
            assert!(error.is_truncated());
            assert!(error.message().ends_with("… [truncated]"));
        })
        .unwrap();
}

#[test]
fn complete_messages_do_not_confuse_capacity_with_encoded_length() {
    let mut runtime = Runtime::new().unwrap();
    for (source, expected) in [
        ("throw ''", String::new()),
        ("throw 'a\\0b'", "a\0b".to_owned()),
        ("throw 'x'.repeat(4096)", "x".repeat(4096)),
        ("throw 'é'.repeat(2048)", "é".repeat(2048)),
        ("throw '😀'.repeat(1024)", "😀".repeat(1024)),
        ("throw '… [truncated]'", "… [truncated]".to_owned()),
    ] {
        let error = thrown(&mut runtime, source);
        assert_eq!(error.message(), expected);
        assert!(!error.is_truncated());
    }
}

#[test]
fn truncated_multibyte_messages_keep_valid_character_boundaries() {
    let mut runtime = Runtime::new().unwrap();
    for source in [
        "throw 'x'.repeat(4097)",
        "throw 'é'.repeat(3000)",
        "throw '😀'.repeat(2048)",
        "throw 'x'.repeat(4095) + '😀'",
    ] {
        let error = thrown(&mut runtime, source);
        assert!(error.is_truncated());
        assert!(error.message().len() <= 4096);
        assert!(error.message.capacity() <= 4097);
        assert!(error.message().ends_with("… [truncated]"));
        assert!(!error.message().contains('\u{fffd}'));
    }
}

#[test]
fn common_evaluation_preserves_truncation_and_string_conversion_stays_strict() {
    let mut runtime = Runtime::new().unwrap();
    runtime
        .with_backend(|backend| {
            let scope = backend.open_scope().unwrap();
            let error = scope
                .evaluate("throw 'x'.repeat(512 * 1024)", "common.js")
                .unwrap_err();
            let BackendError::Exception(error) = error else {
                panic!("expected exception");
            };
            assert!(error.is_truncated());
            assert!(error.message().len() <= 4096);
            let value = scope
                .evaluate(
                    "({ toString() { throw 'y'.repeat(512 * 1024); } })",
                    "coercion.js",
                )
                .unwrap();
            assert_eq!(
                scope.to_string(value).unwrap_err(),
                BackendError::Type {
                    expected: ValueKind::String,
                    actual: ValueKind::Object,
                }
            );
        })
        .unwrap();
}

#[test]
fn callback_exception_snapshots_are_bounded_without_truncating_values() {
    let mut runtime = Runtime::new().unwrap();
    runtime
        .with_context(|cx| {
            let function = cx
                .install_host_function("fail", |_| Err(HostError::new("x".repeat(512 * 1024))))
                .unwrap();
            let JsError::Exception(error) = cx.call(&function, &[]).unwrap_err() else {
                panic!("expected callback exception");
            };
            assert!(error.is_truncated());
            assert!(error.message().len() <= 4096);
            let value = cx.eval("'x'.repeat(512 * 1024)", "value.js").unwrap();
            assert_eq!(cx.string(&value).unwrap().len(), 512 * 1024);
        })
        .unwrap();
}

#[test]
fn publication_rollback_preserves_bounded_exception_metadata() {
    let mut runtime = Runtime::new().unwrap();
    runtime.with_context(|cx| {
        cx.eval(
            "Object.defineProperty(globalThis, 'reject', { set(_) { throw 'x'.repeat(512 * 1024); } });",
            "setter.js",
        ).unwrap();
        let error = cx.install_host_function("reject", |_| Ok(super::Value::Undefined))
            .err().expect("publication must fail");
        let JsError::Exception(error) = error else {
            panic!("expected publication exception");
        };
        assert!(error.is_truncated());
        assert!(error.message().len() <= 4096);
        assert!(cx.shared.host_functions.borrow().is_empty());
        let error = cx.install_native_state("reject", 42_u32)
            .expect_err("publication must fail");
        let JsError::Exception(error) = error else {
            panic!("expected native publication exception");
        };
        assert!(error.is_truncated());
        assert!(error.message().len() <= 4096);
    }).unwrap();
}

#[test]
fn throwing_exception_conversion_uses_the_existing_fallback() {
    let mut runtime = Runtime::new().unwrap();
    let error = thrown(&mut runtime, "throw { toString() { throw 'nested'; } }");
    assert_eq!(
        error.message(),
        "exception could not be converted to a string"
    );
    assert!(!error.is_truncated());
    runtime.invalidate().unwrap();
    assert_eq!(
        error.message(),
        "exception could not be converted to a string"
    );
}
