// SPDX-License-Identifier: MIT OR Apache-2.0

//! Shared callback workloads for timing and allocation probes.

use std::hint::black_box;

use rustjsi_backend_jsc::{Runtime, Value as RustJsiValue};

#[derive(Clone, Copy)]
pub(crate) enum CallbackWorkload {
    Reused,
    Prepared,
    RustJsi,
}

impl CallbackWorkload {
    pub(crate) const fn labels(self) -> (&'static str, &'static str) {
        match self {
            Self::Reused => ("reused", "direct_jsc_lower_bound"),
            Self::Prepared => ("prepared", "direct_jsc_prepared_call"),
            Self::RustJsi => ("rustjsi", "rustjsi_experimental"),
        }
    }
}

#[allow(dead_code)]
pub(crate) fn selected_order() -> [CallbackWorkload; 3] {
    let Ok(value) = std::env::var("RUSTJSI_CALLBACK_ORDER") else {
        return [
            CallbackWorkload::Reused,
            CallbackWorkload::Prepared,
            CallbackWorkload::RustJsi,
        ];
    };
    let reused = CallbackWorkload::Reused;
    let prepared = CallbackWorkload::Prepared;
    let rustjsi = CallbackWorkload::RustJsi;
    match value.as_str() {
        "reused,prepared,rustjsi" => [reused, prepared, rustjsi],
        "reused,rustjsi,prepared" => [reused, rustjsi, prepared],
        "prepared,reused,rustjsi" => [prepared, reused, rustjsi],
        "prepared,rustjsi,reused" => [prepared, rustjsi, reused],
        "rustjsi,reused,prepared" => [rustjsi, reused, prepared],
        "rustjsi,prepared,reused" => [rustjsi, prepared, reused],
        _ => panic!("invalid RUSTJSI_CALLBACK_ORDER: {value}"),
    }
}

pub(crate) fn with_operation<R>(
    workload: CallbackWorkload,
    consume: impl FnOnce(&mut dyn FnMut()) -> R,
) -> R {
    match workload {
        CallbackWorkload::Reused => raw::with_reused(consume),
        CallbackWorkload::Prepared => raw::with_prepared(consume),
        CallbackWorkload::RustJsi => with_rustjsi(consume),
    }
}

fn with_rustjsi<R>(consume: impl FnOnce(&mut dyn FnMut()) -> R) -> R {
    let mut runtime = Runtime::new().expect("create callback benchmark runtime");
    runtime
        .with_context(|context| {
            let add = context
                .install_host_function("rustAdd", |call| {
                    Ok(RustJsiValue::Number(call.number(0)? + call.number(1)?))
                })
                .expect("install host function");
            let arguments = [RustJsiValue::Number(20.0), RustJsiValue::Number(22.0)];
            let result = context.call(&add, &arguments).expect("preflight call");
            assert_answer(context.number(&result).expect("read preflight result"));

            let mut operation = || {
                black_box(context.call(&add, &arguments).expect("callback call"));
            };
            let output = consume(&mut operation);

            let result = context.call(&add, &arguments).expect("postflight call");
            assert_answer(context.number(&result).expect("read postflight result"));
            output
        })
        .expect("enter callback benchmark runtime")
}

fn assert_answer(value: f64) {
    assert!(
        (value - 42.0).abs() < f64::EPSILON,
        "wrong callback result: {value}"
    );
}

mod raw {
    use std::ffi::c_void;
    use std::hint::black_box;
    use std::ptr;

    type Context = *const c_void;
    type GlobalContext = *mut c_void;
    type Value = *const c_void;
    type Object = *mut c_void;
    type Callback = Option<
        unsafe extern "C" fn(Context, Object, Object, usize, *const Value, *mut Value) -> Value,
    >;

    #[link(name = "JavaScriptCore", kind = "framework")]
    unsafe extern "C" {
        #[link_name = "JSGlobalContextCreate"]
        fn context_create(class: *mut c_void) -> GlobalContext;

        #[link_name = "JSGlobalContextRelease"]
        fn context_release(context: GlobalContext);

        #[link_name = "JSObjectMakeFunctionWithCallback"]
        fn make_function(context: Context, name: *mut c_void, callback: Callback) -> Object;

        #[link_name = "JSObjectCallAsFunction"]
        fn call_function(
            context: Context,
            function: Object,
            this_object: Object,
            argument_count: usize,
            arguments: *const Value,
            exception: *mut Value,
        ) -> Value;

        #[link_name = "JSValueMakeNumber"]
        fn make_number(context: Context, number: f64) -> Value;

        #[link_name = "JSValueIsNumber"]
        fn is_number(context: Context, value: Value) -> bool;

        #[link_name = "JSValueToNumber"]
        fn to_number(context: Context, value: Value, exception: *mut Value) -> f64;

        #[link_name = "JSValueProtect"]
        fn protect(context: Context, value: Value);

        #[link_name = "JSValueUnprotect"]
        fn unprotect(context: Context, value: Value);
    }

    struct OwnedContext(GlobalContext);

    impl OwnedContext {
        fn new() -> Self {
            // SAFETY: A null class requests the default global. Check before use.
            let context = unsafe { context_create(ptr::null_mut()) };
            assert!(!context.is_null(), "create direct JSC context");
            Self(context)
        }
    }

    impl Drop for OwnedContext {
        fn drop(&mut self) {
            // SAFETY: This guard uniquely owns one successful creation on this
            // thread. All borrowing function guards have already been dropped.
            unsafe { context_release(self.0) };
        }
    }

    struct RootedFunction<'context> {
        context: &'context OwnedContext,
        function: Object,
    }

    impl<'context> RootedFunction<'context> {
        fn new(context: &'context OwnedContext) -> Self {
            // SAFETY: The context is live and raw_add matches the synchronous ABI.
            let function = unsafe { make_function(context.0, ptr::null_mut(), Some(raw_add)) };
            assert!(!function.is_null(), "create direct JSC function");
            // SAFETY: Root immediately, before further engine work. This guard
            // balances the protection before its borrowed context can be released.
            unsafe { protect(context.0, function) };
            Self { context, function }
        }
    }

    impl Drop for RootedFunction<'_> {
        fn drop(&mut self) {
            // SAFETY: The owning context borrow is still live on this thread,
            // and this non-cloneable guard owns exactly one protection.
            unsafe { unprotect(self.context.0, self.function) };
        }
    }

    pub(super) fn with_reused<R>(consume: impl FnOnce(&mut dyn FnMut()) -> R) -> R {
        let owner = OwnedContext::new();
        let rooted = RootedFunction::new(&owner);
        let context = owner.0;
        let function = rooted.function;
        // SAFETY: Both primitive values are created in the live context.
        let arguments = unsafe { [make_number(context, 20.0), make_number(context, 22.0)] };
        check_result(context, call(context, function, &arguments));

        let mut operation = || {
            black_box(call(context, function, &arguments));
        };
        let output = consume(&mut operation);

        check_result(context, call(context, function, &arguments));
        output
    }

    pub(super) fn with_prepared<R>(consume: impl FnOnce(&mut dyn FnMut()) -> R) -> R {
        let owner = OwnedContext::new();
        let rooted = RootedFunction::new(&owner);
        let context = owner.0;
        let function = rooted.function;
        check_result(context, call_with_prepared_arguments(context, function));

        let mut operation = || {
            black_box(call_with_prepared_arguments(context, function));
        };
        let output = consume(&mut operation);

        check_result(context, call_with_prepared_arguments(context, function));
        output
    }

    fn check_result(context: Context, value: Value) {
        assert!(!value.is_null(), "direct call returned null");
        // SAFETY: The synchronous call just returned this value in its live context.
        assert!(
            unsafe { is_number(context, value) },
            "direct call returned a non-number"
        );
        let mut exception = ptr::null();
        // SAFETY: The strict check avoids coercion; capture the exception output.
        let number = unsafe { to_number(context, value, &raw mut exception) };
        assert!(exception.is_null(), "direct result conversion threw");
        super::assert_answer(number);
    }

    fn call_with_prepared_arguments(context: Context, function: Object) -> Value {
        // SAFETY: Both primitive arguments are created in the live context and
        // remain valid for the immediately following synchronous call.
        let arguments = unsafe {
            [
                make_number(context, black_box(20.0)),
                make_number(context, black_box(22.0)),
            ]
        };
        call(context, function, &arguments)
    }

    fn call(context: Context, function: Object, arguments: &[Value; 2]) -> Value {
        let mut exception = ptr::null();
        // SAFETY: The function and arguments belong to `context` and remain live for
        // this synchronous call. The exception output is checked.
        let result = unsafe {
            call_function(
                context,
                function,
                ptr::null_mut(),
                arguments.len(),
                arguments.as_ptr(),
                &raw mut exception,
            )
        };
        assert!(exception.is_null(), "direct JSC callback threw");
        result
    }

    unsafe extern "C" fn raw_add(
        context: Context,
        _function: Object,
        _this_object: Object,
        argument_count: usize,
        arguments: *const Value,
        _exception: *mut Value,
    ) -> Value {
        if argument_count != 2 || arguments.is_null() {
            // SAFETY: JSC supplied the live callback context.
            return unsafe { make_number(context, f64::NAN) };
        }
        // SAFETY: JSC supplied two argument handles for this callback frame.
        let arguments = unsafe { std::slice::from_raw_parts(arguments, argument_count) };
        // SAFETY: All values and the context are live for this callback.
        if !unsafe { is_number(context, arguments[0]) }
            || !unsafe { is_number(context, arguments[1]) }
        {
            // SAFETY: JSC supplied the live callback context.
            return unsafe { make_number(context, f64::NAN) };
        }
        let mut exception = ptr::null();
        // SAFETY: The strict checks above avoid user coercion; exception outputs are
        // still provided for ABI equivalence with the wrapped path.
        let left = unsafe { to_number(context, arguments[0], &raw mut exception) };
        let right = unsafe { to_number(context, arguments[1], &raw mut exception) };
        // SAFETY: JSC supplied the live callback context.
        unsafe { make_number(context, left + right) }
    }
}
