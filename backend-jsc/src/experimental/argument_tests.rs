// SPDX-License-Identifier: MIT OR Apache-2.0

use super::{HostError, JsError, RootLimits, Runtime, RuntimeError, Value};
use std::cell::Cell;
use std::rc::Rc;

#[test]
fn heap_arguments_have_call_scoped_roots() {
    let mut runtime = Runtime::new().unwrap();
    let observed = Rc::new(Cell::new(0));
    let shared = Rc::clone(&runtime.shared);
    runtime
        .with_context(|cx| {
            let snapshot = Rc::clone(&observed);
            let weak = Rc::downgrade(&shared);
            let function = cx
                .install_host_function("inspectArguments", move |_| {
                    snapshot.set(weak.upgrade().unwrap().argument_roots.get());
                    Ok(Value::Undefined)
                })
                .unwrap();
            let arguments: Vec<_> = (0..128)
                .map(|i| Value::String(format!("argument-{i}")))
                .collect();
            cx.call(&function, &arguments).unwrap();
            assert_eq!(observed.get(), 128);
            assert_eq!(shared.argument_roots.get(), 0);
        })
        .unwrap();
}

#[test]
fn forced_collection_during_argument_preparation_preserves_strings() {
    let mut runtime = Runtime::new().unwrap();
    let expected: Rc<Vec<_>> = Rc::new(
        (0..64)
            .map(|i| format!("value-{i}-{}", "x".repeat(64)))
            .collect(),
    );
    let arguments: Vec<_> = expected.iter().cloned().map(Value::String).collect();
    let callback_expected = Rc::clone(&expected);
    let shared = Rc::clone(&runtime.shared);
    runtime
        .with_context(|cx| {
            let function = cx
                .install_host_function("verifyArguments", move |call| {
                    assert_eq!(call.len(), callback_expected.len());
                    for (index, expected) in callback_expected.iter().enumerate() {
                        assert_eq!(call.string(index)?, *expected);
                    }
                    Ok(Value::Number(64.0))
                })
                .unwrap();
            shared.argument_gc.set(true);
            let result = cx.call(&function, &arguments).unwrap();
            shared.argument_gc.set(false);
            assert_eq!(cx.number(&result).unwrap().to_bits(), 64.0_f64.to_bits());
            assert_eq!(shared.argument_roots.get(), 0);
            assert_eq!(shared.local_budget.used(), 0);
        })
        .unwrap();
}

#[test]
fn argument_admission_is_atomic_and_refunds_the_result_slot() {
    let mut runtime = Runtime::new_with_root_limits(RootLimits {
        persistent_slots: 1,
        local_roots: 3,
    })
    .unwrap();
    let calls = Rc::new(Cell::new(0));
    let shared = Rc::clone(&runtime.shared);
    runtime
        .with_context(|cx| {
            let callback_calls = Rc::clone(&calls);
            let function = cx
                .install_host_function("count", move |_| {
                    callback_calls.set(callback_calls.get() + 1);
                    Ok(Value::Undefined)
                })
                .unwrap();
            let two = [Value::String("one".into()), Value::String("two".into())];
            cx.call(&function, &two).unwrap();
            assert_eq!(calls.get(), 1);
            assert_eq!(shared.local_budget.used(), 0);
            let three = [
                Value::String("one".into()),
                Value::String("two".into()),
                Value::String("three".into()),
            ];
            assert_eq!(
                cx.call(&function, &three).unwrap_err(),
                JsError::Runtime(RuntimeError::LocalRootLimitReached)
            );
            assert_eq!(calls.get(), 1);
            assert_eq!(shared.argument_roots.get(), 0);
            assert_eq!(shared.local_budget.used(), 0);
        })
        .unwrap();
}

#[test]
fn string_argument_roots_release_after_result_or_exception_capture() {
    let mut runtime = Runtime::new().unwrap();
    let shared = Rc::clone(&runtime.shared);
    runtime
        .with_context(|cx| {
            let echo = cx
                .install_host_function("echo", |call| Ok(Value::String(call.string(0)?)))
                .unwrap();
            let fail = cx
                .install_host_function("fail", |_| Err(HostError::new("expected")))
                .unwrap();
            let argument = [Value::String("kept through call".into())];
            let result = cx.call(&echo, &argument).unwrap();
            assert_eq!(cx.string(&result).unwrap(), "kept through call");
            assert_eq!(shared.argument_roots.get(), 0);
            assert_eq!(shared.local_budget.used(), 1);
            assert!(matches!(
                cx.call(&fail, &argument),
                Err(JsError::Exception(_))
            ));
            assert_eq!(shared.argument_roots.get(), 0);
            assert_eq!(shared.local_budget.used(), 1);
        })
        .unwrap();
    assert_eq!(shared.local_budget.used(), 0);
}

#[test]
fn scalar_arguments_do_not_consume_temporary_root_capacity() {
    let mut runtime = Runtime::new_with_root_limits(RootLimits {
        persistent_slots: 1,
        local_roots: 1,
    })
    .unwrap();
    let shared = Rc::clone(&runtime.shared);
    runtime
        .with_context(|cx| {
            let function = cx
                .install_host_function("sum", |call| {
                    Ok(Value::Number(call.number(0)? + call.number(1)?))
                })
                .unwrap();
            let result = cx
                .call(&function, &[Value::Number(20.0), Value::Number(22.0)])
                .unwrap();
            assert_eq!(cx.number(&result).unwrap().to_bits(), 42.0_f64.to_bits());
            assert_eq!(shared.argument_roots.get(), 0);
            assert_eq!(shared.local_budget.used(), 0);
        })
        .unwrap();
}

#[test]
fn temporary_argument_roots_and_capacity_drop_during_unwind() {
    let mut runtime = Runtime::new().unwrap();
    let shared = Rc::clone(&runtime.shared);
    runtime
        .with_context(|cx| {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _reservation = shared.local_budget.reserve_many(2).unwrap();
                let (_, first) = cx.prepare_argument(&Value::String("first".into())).unwrap();
                let (_, second) = cx
                    .prepare_argument(&Value::String("second".into()))
                    .unwrap();
                let _roots = [first.unwrap(), second.unwrap()];
                assert_eq!(shared.argument_roots.get(), 2);
                panic!("argument preparation interrupted");
            }));
            assert!(result.is_err());
            assert_eq!(shared.argument_roots.get(), 0);
            assert_eq!(shared.local_budget.used(), 0);
            cx.collect_garbage().unwrap();
            let value = cx.eval("42", "after-unwind.js").unwrap();
            assert_eq!(cx.number(&value).unwrap().to_bits(), 42.0_f64.to_bits());
        })
        .unwrap();
}
