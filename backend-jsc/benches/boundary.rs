// SPDX-License-Identifier: MIT OR Apache-2.0

//! Direct JSC host-function boundary microbenchmark.

#[cfg(target_os = "macos")]
fn main() {
    use rustjsi_backend_jsc::{Runtime, Value};
    use std::hint::black_box;
    use std::time::Instant;

    const WARMUP: u32 = 10_000;
    const ITERATIONS: u32 = 1_000_000;

    let direct = raw::measure(WARMUP, ITERATIONS);

    let mut runtime = Runtime::new().expect("create RustJSI JSC runtime");
    let mut rustjsi = 0.0;
    runtime
        .with_context(|context| {
            let add = context
                .install_host_function("rustAdd", |call| {
                    Ok(Value::Number(call.number(0)? + call.number(1)?))
                })
                .expect("install host function");
            let arguments = [Value::Number(20.0), Value::Number(22.0)];

            for _ in 0..WARMUP {
                black_box(context.call(&add, &arguments).expect("warmup call"));
            }

            let started = Instant::now();
            for _ in 0..ITERATIONS {
                black_box(context.call(&add, &arguments).expect("measured call"));
            }
            let elapsed = started.elapsed();
            rustjsi = elapsed.as_secs_f64() * 1_000_000_000.0 / f64::from(ITERATIONS);
        })
        .expect("enter JSC runtime");

    println!("direct_jsc_lower_bound: {direct:.2} ns/call");
    println!("rustjsi_experimental: {rustjsi:.2} ns/call");
    println!(
        "rustjsi_over_direct: {:.3}x ({ITERATIONS} iterations)",
        rustjsi / direct
    );
}

#[cfg(not(target_os = "macos"))]
fn main() {}

#[cfg(target_os = "macos")]
mod raw {
    use std::ffi::c_void;
    use std::hint::black_box;
    use std::ptr;
    use std::time::Instant;

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
    }

    pub(super) fn measure(warmup: u32, iterations: u32) -> f64 {
        // SAFETY: The default global class is requested and the returned context is
        // checked before any use.
        let context = unsafe { context_create(ptr::null_mut()) };
        assert!(!context.is_null(), "create direct JSC context");
        // SAFETY: `raw_add` matches JSC's synchronous callback ABI.
        let function = unsafe { make_function(context, ptr::null_mut(), Some(raw_add)) };
        assert!(!function.is_null(), "create direct JSC function");
        // SAFETY: Both primitive values are created in the live context.
        let arguments = unsafe { [make_number(context, 20.0), make_number(context, 22.0)] };

        for _ in 0..warmup {
            black_box(call(context, function, &arguments));
        }
        let started = Instant::now();
        for _ in 0..iterations {
            black_box(call(context, function, &arguments));
        }
        let elapsed = started.elapsed();

        // SAFETY: This balances the single successful context creation after all
        // synchronous calls have returned.
        unsafe { context_release(context) };
        elapsed.as_secs_f64() * 1_000_000_000.0 / f64::from(iterations)
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
