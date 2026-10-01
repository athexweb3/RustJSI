// SPDX-License-Identifier: MIT OR Apache-2.0

use super::{InboundCallbackLimits, JscRuntimeLimits, Runtime, Value};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

fn limits(arguments: usize, string_utf8_bytes: usize) -> JscRuntimeLimits {
    JscRuntimeLimits {
        inbound_callback: InboundCallbackLimits {
            arguments,
            string_utf8_bytes,
        },
        ..JscRuntimeLimits::default()
    }
}

#[test]
fn inbound_string_data_limit_accepts_complete_utf8_and_rejects_partial_data() {
    let mut runtime = Runtime::new_with_jsc_limits(limits(4, 4)).unwrap();
    let observed = Rc::new(RefCell::new(Vec::new()));

    runtime
        .with_context(|cx| {
            let callback_observed = Rc::clone(&observed);
            cx.install_host_function("captureInbound", move |call| {
                let value = call.string(0)?;
                callback_observed.borrow_mut().push(value.clone());
                Ok(Value::String(value))
            })
            .unwrap();

            let ascii = cx
                .eval("captureInbound('rust')", "inbound-ascii.js")
                .unwrap();
            assert_eq!(cx.string(&ascii).unwrap(), "rust");

            let non_bmp = cx
                .eval("captureInbound('🦀')", "inbound-non-bmp.js")
                .unwrap();
            assert_eq!(cx.string(&non_bmp).unwrap(), "🦀");

            let rejected = cx
                .eval("captureInbound('🦀x')", "inbound-too-large.js")
                .unwrap_err();
            assert!(
                rejected
                    .to_string()
                    .contains("inbound callback string data limit reached")
            );
            assert_eq!(&*observed.borrow(), &["rust", "🦀"]);

            let after_rejection = cx
                .eval("captureInbound('ok')", "inbound-after-rejection.js")
                .unwrap();
            assert_eq!(cx.string(&after_rejection).unwrap(), "ok");
            assert_eq!(&*observed.borrow(), &["rust", "🦀", "ok"]);
        })
        .unwrap();
}

#[test]
fn inbound_argument_limit_rejects_before_callback_invocation() {
    let mut runtime = Runtime::new_with_jsc_limits(limits(1, usize::MAX)).unwrap();
    let calls = Rc::new(Cell::new(0));

    runtime
        .with_context(|cx| {
            let callback_calls = Rc::clone(&calls);
            cx.install_host_function("countInbound", move |_| {
                callback_calls.set(callback_calls.get() + 1);
                Ok(Value::Undefined)
            })
            .unwrap();

            let rejected = cx
                .eval("countInbound(1, 2)", "inbound-too-many-arguments.js")
                .unwrap_err();
            assert!(
                rejected
                    .to_string()
                    .contains("inbound callback argument limit reached")
            );
            assert_eq!(calls.get(), 0);

            cx.eval("countInbound(1)", "inbound-argument-after-rejection.js")
                .unwrap();
            assert_eq!(calls.get(), 1);
        })
        .unwrap();
}

#[test]
fn inbound_string_data_limit_is_cumulative_within_one_callback() {
    let mut runtime = Runtime::new_with_jsc_limits(limits(2, 4)).unwrap();
    let observed = Rc::new(RefCell::new(Vec::new()));

    runtime
        .with_context(|cx| {
            let callback_observed = Rc::clone(&observed);
            cx.install_host_function("joinInbound", move |call| {
                let joined = format!("{}{}", call.string(0)?, call.string(1)?);
                callback_observed.borrow_mut().push(joined.clone());
                Ok(Value::String(joined))
            })
            .unwrap();

            let exact = cx
                .eval("joinInbound('ab', 'cd')", "inbound-cumulative-exact.js")
                .unwrap();
            assert_eq!(cx.string(&exact).unwrap(), "abcd");

            let rejected = cx
                .eval(
                    "joinInbound('ab', 'cde')",
                    "inbound-cumulative-too-large.js",
                )
                .unwrap_err();
            assert!(
                rejected
                    .to_string()
                    .contains("inbound callback string data limit reached")
            );
            assert_eq!(&*observed.borrow(), &["abcd"]);
        })
        .unwrap();
}

#[test]
fn inbound_string_coercion_exception_remains_distinct_from_admission_rejection() {
    let mut runtime = Runtime::new_with_jsc_limits(limits(1, 4)).unwrap();

    runtime
        .with_context(|cx| {
            cx.install_host_function("coerceInbound", |call| {
                let _ = call.string(0)?;
                Ok(Value::Undefined)
            })
            .unwrap();

            let error = cx
                .eval(
                    "coerceInbound({ toString() { throw new Error('coercion failure'); } })",
                    "inbound-coercion.js",
                )
                .unwrap_err()
                .to_string();
            assert!(error.contains("coercion failure"));
            assert!(!error.contains("inbound callback string data limit reached"));
        })
        .unwrap();
}
