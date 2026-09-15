// SPDX-License-Identifier: MIT OR Apache-2.0

//! Shared pure-JavaScript call workloads for timing and allocation probes.

use rustjsi_backend::{BackendBase, BackendScope, CallReceiver};
use rustjsi_backend_jsc::Runtime;
use std::ffi::c_void;
use std::hint::black_box;
use std::ptr;

const SOURCE: &str = "(function(a, b) { return a + b; })";

#[derive(Clone, Copy)]
pub(crate) enum JsCallWorkload {
    Direct,
    Common,
}

impl JsCallWorkload {
    pub(crate) const fn labels(self) -> (&'static str, &'static str) {
        match self {
            Self::Direct => ("direct", "direct_jsc_js_call"),
            Self::Common => ("common", "rustjsi_common_js_call"),
        }
    }
}

#[allow(dead_code)]
pub(crate) fn selected_order() -> [JsCallWorkload; 2] {
    let direct = JsCallWorkload::Direct;
    let common = JsCallWorkload::Common;
    match std::env::var("RUSTJSI_JS_CALL_ORDER").as_deref() {
        Ok("direct,common") | Err(_) => [direct, common],
        Ok("common,direct") => [common, direct],
        Ok(value) => panic!("invalid RUSTJSI_JS_CALL_ORDER: {value}"),
    }
}

pub(crate) fn with_operation<R>(
    workload: JsCallWorkload,
    consume: impl FnOnce(&mut dyn FnMut()) -> R,
) -> R {
    match workload {
        JsCallWorkload::Direct => raw::with_operation(consume),
        JsCallWorkload::Common => with_common_operation(consume),
    }
}

fn with_common_operation<R>(consume: impl FnOnce(&mut dyn FnMut()) -> R) -> R {
    let mut runtime = Runtime::new().expect("create common call benchmark runtime");
    runtime
        .with_backend(|backend| {
            let scope = backend.open_scope().expect("open common call scope");
            let function = scope
                .evaluate(SOURCE, "js-calls.js")
                .expect("evaluate common call function");
            let invoke = || {
                let arguments = [
                    scope.number(black_box(20.0)).expect("make left number"),
                    scope.number(black_box(22.0)).expect("make right number"),
                ];
                let result = scope
                    .call(function, CallReceiver::Global, &arguments)
                    .expect("call common JavaScript function");
                scope.as_number(result).expect("read common call result")
            };
            assert_answer(invoke());

            let mut operation = || {
                black_box(invoke());
            };
            let output = consume(&mut operation);

            assert_answer(invoke());
            output
        })
        .expect("enter common call benchmark runtime")
}

fn assert_answer(value: f64) {
    assert_eq!(value.to_bits(), 42.0_f64.to_bits(), "wrong call result");
}

mod raw {
    use super::{SOURCE, assert_answer, black_box, c_void, ptr};

    type Handle = *const c_void;
    type Object = *mut c_void;

    #[link(name = "JavaScriptCore", kind = "framework")]
    unsafe extern "C" {
        fn JSGlobalContextCreate(class: Object) -> Object;
        fn JSGlobalContextRelease(context: Object);
        fn JSStringCreateWithCharacters(chars: *const u16, len: usize) -> Object;
        fn JSStringRelease(string: Object);
        fn JSEvaluateScript(
            context: Handle,
            script: Object,
            this: Object,
            url: Object,
            line: i32,
            exception: *mut Handle,
        ) -> Handle;
        fn JSValueProtect(context: Handle, value: Handle);
        fn JSValueUnprotect(context: Handle, value: Handle);
        fn JSValueGetType(context: Handle, value: Handle) -> i32;
        fn JSValueIsNumber(context: Handle, value: Handle) -> bool;
        fn JSObjectIsFunction(context: Handle, value: Object) -> bool;
        fn JSValueMakeNumber(context: Handle, number: f64) -> Handle;
        fn JSValueToNumber(context: Handle, value: Handle, exception: *mut Handle) -> f64;
        fn JSObjectCallAsFunction(
            context: Handle,
            function: Object,
            this: Object,
            count: usize,
            args: *const Handle,
            exception: *mut Handle,
        ) -> Handle;
    }

    struct Owner(Object);

    impl Owner {
        fn new() -> Self {
            // SAFETY: Null selects the default class; creation is checked.
            let context = unsafe { JSGlobalContextCreate(ptr::null_mut()) };
            assert!(!context.is_null(), "create direct call context");
            Self(context)
        }
    }

    impl Drop for Owner {
        fn drop(&mut self) {
            // SAFETY: This guard uniquely owns the context on its creating thread.
            unsafe { JSGlobalContextRelease(self.0) };
        }
    }

    struct StringOwner(Object);

    impl Drop for StringOwner {
        fn drop(&mut self) {
            // SAFETY: This guard owns one successful string creation.
            unsafe { JSStringRelease(self.0) };
        }
    }

    struct Function<'context> {
        owner: &'context Owner,
        handle: Handle,
    }

    impl Drop for Function<'_> {
        fn drop(&mut self) {
            // SAFETY: The borrowed owner outlives this single protection.
            unsafe { JSValueUnprotect(self.owner.0, self.handle) };
        }
    }

    pub(super) fn with_operation<R>(consume: impl FnOnce(&mut dyn FnMut()) -> R) -> R {
        let owner = Owner::new();
        let source: Vec<u16> = SOURCE.encode_utf16().collect();
        // SAFETY: JSC copies the initialized UTF-16 slice.
        let script = unsafe { JSStringCreateWithCharacters(source.as_ptr(), source.len()) };
        assert!(!script.is_null(), "create direct call source");
        let script = StringOwner(script);
        let mut exception = ptr::null();
        // SAFETY: The context and string remain live; exception is checked.
        let function = unsafe {
            JSEvaluateScript(
                owner.0,
                script.0,
                ptr::null_mut(),
                ptr::null_mut(),
                1,
                &raw mut exception,
            )
        };
        assert!(exception.is_null() && !function.is_null());
        // SAFETY: Root the evaluation result before further engine work.
        unsafe { JSValueProtect(owner.0, function) };
        let function = Function {
            owner: &owner,
            handle: function,
        };
        assert_answer(invoke(owner.0, function.handle));

        let mut operation = || {
            black_box(invoke(owner.0, function.handle));
        };
        let output = consume(&mut operation);

        assert_answer(invoke(owner.0, function.handle));
        output
    }

    fn invoke(context: Handle, function: Handle) -> f64 {
        // SAFETY: The context and rooted function are live on this thread. Type
        // checks precede object use, and exception outputs are checked.
        unsafe {
            let arguments = [
                JSValueMakeNumber(context, black_box(20.0)),
                JSValueMakeNumber(context, black_box(22.0)),
            ];
            assert!(arguments.iter().all(|argument| !argument.is_null()));
            assert_eq!(JSValueGetType(context, function), 5);
            assert!(JSObjectIsFunction(context, function.cast_mut()));
            let mut exception = ptr::null();
            let result = JSObjectCallAsFunction(
                context,
                function.cast_mut(),
                ptr::null_mut(),
                arguments.len(),
                arguments.as_ptr(),
                &raw mut exception,
            );
            assert!(exception.is_null() && !result.is_null());
            assert_eq!(JSValueGetType(context, result), 3);
            assert!(JSValueIsNumber(context, result));
            let number = JSValueToNumber(context, result, &raw mut exception);
            assert!(exception.is_null());
            number
        }
    }
}
