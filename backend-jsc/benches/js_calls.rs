// SPDX-License-Identifier: MIT OR Apache-2.0

//! Prepared scalar calls to the same JavaScript function through two APIs.

#[cfg(target_os = "macos")]
mod implementation {
    use rustjsi_backend::{BackendBase, BackendScope, CallReceiver};
    use rustjsi_backend_jsc::Runtime;
    use std::ffi::c_void;
    use std::hint::black_box;
    use std::ptr;
    use std::time::Instant;

    type Handle = *const c_void;
    type Object = *mut c_void;
    const SOURCE: &str = "(function(a, b) { return a + b; })";
    const ITERATIONS: u32 = 1_000_000;

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
    impl Drop for Owner {
        fn drop(&mut self) {
            // SAFETY: This guard uniquely owns the context on its creating thread.
            unsafe { JSGlobalContextRelease(self.0) };
        }
    }
    struct StringOwner(Object);
    impl Drop for StringOwner {
        fn drop(&mut self) {
            // SAFETY: This guard owns the successful string creation.
            unsafe { JSStringRelease(self.0) };
        }
    }
    struct Function<'a>(&'a Owner, Handle);
    impl Drop for Function<'_> {
        fn drop(&mut self) {
            // SAFETY: The borrowed owner outlives this single protection.
            unsafe { JSValueUnprotect(self.0.0, self.1) };
        }
    }

    fn measure(name: &str, mut operation: impl FnMut() -> f64) {
        assert_eq!(operation().to_bits(), 42.0_f64.to_bits());
        for _ in 0..100_000 {
            black_box(operation());
        }
        let start = Instant::now();
        for _ in 0..ITERATIONS {
            black_box(operation());
        }
        let elapsed = start.elapsed();
        assert_eq!(operation().to_bits(), 42.0_f64.to_bits());
        println!(
            "{name}: {:.3} ns/call ({ITERATIONS} iterations)",
            elapsed.as_secs_f64() * 1e9 / f64::from(ITERATIONS)
        );
    }

    fn direct() {
        // SAFETY: Null selects the default class; creation is checked immediately.
        let context = unsafe { JSGlobalContextCreate(ptr::null_mut()) };
        assert!(!context.is_null());
        let owner = Owner(context);
        let source: Vec<u16> = SOURCE.encode_utf16().collect();
        // SAFETY: JSC copies the initialized UTF-16 slice.
        let script = unsafe { JSStringCreateWithCharacters(source.as_ptr(), source.len()) };
        assert!(!script.is_null());
        let script = StringOwner(script);
        let mut exception = ptr::null();
        // SAFETY: Strings and context remain live; exception is checked.
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
        // SAFETY: Root the evaluation result before further engine calls.
        unsafe { JSValueProtect(owner.0, function) };
        let function = Function(&owner, function);
        measure("direct_js_call", || {
            // SAFETY: The context and rooted function are live on this thread;
            // scalars are immediate values. Type checks precede object use.
            unsafe {
                let arguments = [
                    JSValueMakeNumber(owner.0, black_box(20.0)),
                    JSValueMakeNumber(owner.0, black_box(22.0)),
                ];
                assert_eq!(JSValueGetType(owner.0, function.1), 5);
                assert!(JSObjectIsFunction(owner.0, function.1.cast_mut()));
                let mut exception = ptr::null();
                let result = JSObjectCallAsFunction(
                    owner.0,
                    function.1.cast_mut(),
                    ptr::null_mut(),
                    2,
                    arguments.as_ptr(),
                    &raw mut exception,
                );
                assert!(exception.is_null() && !result.is_null());
                // Match the common call's result classification and strict read.
                assert_eq!(JSValueGetType(owner.0, result), 3);
                assert!(JSValueIsNumber(owner.0, result));
                let number = JSValueToNumber(owner.0, result, &raw mut exception);
                assert!(exception.is_null());
                number
            }
        });
    }

    fn common() {
        let mut runtime = Runtime::new().unwrap();
        runtime
            .with_backend(|backend| {
                let scope = backend.open_scope().unwrap();
                let function = scope.evaluate(SOURCE, "js-calls.js").unwrap();
                measure("common_js_call", || {
                    let arguments = [
                        scope.number(black_box(20.0)).unwrap(),
                        scope.number(black_box(22.0)).unwrap(),
                    ];
                    let result = scope
                        .call(function, CallReceiver::Global, &arguments)
                        .unwrap();
                    scope.as_number(result).unwrap()
                });
            })
            .unwrap();
    }

    pub(super) fn run() {
        let order =
            std::env::var("RUSTJSI_JS_CALL_ORDER").unwrap_or_else(|_| "direct,common".into());
        match order.as_str() {
            "direct,common" => {
                direct();
                common();
            }
            "common,direct" => {
                common();
                direct();
            }
            _ => panic!("invalid RUSTJSI_JS_CALL_ORDER: {order}"),
        }
    }
}

fn main() {
    #[cfg(target_os = "macos")]
    implementation::run();
}
