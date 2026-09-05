// SPDX-License-Identifier: MIT OR Apache-2.0

//! Direct JSC host-function boundary microbenchmark.

#[cfg(target_os = "macos")]
#[global_allocator]
static COUNTING_ALLOCATOR: allocation::CountingAllocator = allocation::CountingAllocator;

#[cfg(target_os = "macos")]
fn main() {
    use rustjsi_backend::{BackendBase, BackendScope};
    use rustjsi_backend_jsc::{Attachment, Runtime, Value};
    use rustjsi_host::{EntryGate, FinalEntryPolicy, RuntimeIdentity};
    use std::hint::black_box;
    use std::num::NonZeroU32;
    use std::time::Instant;

    const WARMUP: u32 = 10_000;
    const ITERATIONS: u32 = 1_000_000;
    const ENTRY_BATCHES: u32 = 1_000;

    let direct = raw::measure(WARMUP, ITERATIONS);
    let direct_scalar = raw::measure_scalar(WARMUP, ITERATIONS);

    let mut runtime = Runtime::new().expect("create RustJSI JSC runtime");
    let gate = EntryGate::new(NonZeroU32::new(64).unwrap(), FinalEntryPolicy::Unavailable);
    let gate_entry = measure_entry(WARMUP, ITERATIONS, ENTRY_BATCHES, || {
        let entry = black_box(&gate).try_enter().expect("admit host entry");
        black_box(&entry);
        drop(entry);
    });
    let common_entry = measure_entry(WARMUP, ITERATIONS, ENTRY_BATCHES, || {
        black_box(&mut runtime)
            .with_backend(|_| black_box(()))
            .expect("enter common backend");
    });
    let foreign_owner = raw::OwnedContext::new();
    let mut identity = RuntimeIdentity::allocate().expect("allocate foreign host identity");
    let mut attachment = Attachment::new(&mut identity, FinalEntryPolicy::Guaranteed)
        .expect("create foreign attachment");
    let foreign_common_entry = measure_entry(WARMUP, ITERATIONS, ENTRY_BATCHES, || {
        // SAFETY: The benchmark owner keeps this context live on the current
        // thread and lends the same global context to every entry.
        unsafe {
            black_box(&mut attachment)
                .with_backend(foreign_owner.as_void(), |_| black_box(()))
                .expect("enter foreign common backend");
        }
    });
    let mut rustjsi = 0.0;
    runtime
        .with_context(|context| {
            let add = context
                .install_host_function("rustAdd", |call| {
                    Ok(Value::Number(call.number(0)? + call.number(1)?))
                })
                .expect("install host function");
            let arguments = [Value::Number(20.0), Value::Number(22.0)];
            let result = context.call(&add, &arguments).expect("preflight call");
            assert_answer(context.number(&result).expect("read preflight result"));

            for _ in 0..WARMUP {
                black_box(context.call(&add, &arguments).expect("warmup call"));
            }

            let started = Instant::now();
            for _ in 0..ITERATIONS {
                black_box(context.call(&add, &arguments).expect("measured call"));
            }
            let elapsed = started.elapsed();
            rustjsi = elapsed.as_secs_f64() * 1_000_000_000.0 / f64::from(ITERATIONS);
            let result = context.call(&add, &arguments).expect("postflight call");
            assert_answer(context.number(&result).expect("read postflight result"));
        })
        .expect("enter JSC runtime");

    let mut common_scalar = 0.0;
    runtime
        .with_backend(|backend| {
            let scope = backend.open_scope().expect("open common JSC scope");
            let value = scope.number(42.0).expect("preflight number");
            assert_answer(scope.as_number(value).expect("read preflight number"));
            for _ in 0..WARMUP {
                let value = scope.number(black_box(42.0)).expect("make number");
                black_box(scope.as_number(value).expect("read number"));
            }

            let started = Instant::now();
            for _ in 0..ITERATIONS {
                let value = scope.number(black_box(42.0)).expect("make number");
                black_box(scope.as_number(value).expect("read number"));
            }
            let elapsed = started.elapsed();
            common_scalar = elapsed.as_secs_f64() * 1_000_000_000.0 / f64::from(ITERATIONS);
            let value = scope.number(42.0).expect("postflight number");
            assert_answer(scope.as_number(value).expect("read postflight number"));
        })
        .expect("enter common JSC backend");

    println!("direct_jsc_lower_bound: {direct:.2} ns/call");
    print_entry_measurement("host_gate_admit_and_exit", &gate_entry);
    print_entry_measurement("jsc_common_empty_entry", &common_entry);
    print_entry_measurement("jsc_foreign_common_empty_entry", &foreign_common_entry);
    println!("rustjsi_experimental: {rustjsi:.2} ns/call");
    println!(
        "rustjsi_over_direct: {:.3}x ({ITERATIONS} iterations)",
        rustjsi / direct
    );
    println!("direct_jsc_scalar: {direct_scalar:.2} ns/round-trip");
    println!("rustjsi_common_scalar: {common_scalar:.2} ns/round-trip");
    println!(
        "common_scalar_over_direct: {:.3}x ({ITERATIONS} iterations)",
        common_scalar / direct_scalar
    );
    // SAFETY: This is the same still-live context used for every measured entry.
    let _ = unsafe { attachment.detach_with_context(foreign_owner.as_void()) }
        .expect("detach foreign benchmark attachment");
}

#[cfg(target_os = "macos")]
fn assert_answer(value: f64) {
    assert!(
        (value - 42.0).abs() < f64::EPSILON,
        "wrong benchmark result: {value}"
    );
}

#[cfg(target_os = "macos")]
fn measure_entry(
    warmup: u32,
    iterations: u32,
    batches: u32,
    mut operation: impl FnMut(),
) -> EntryMeasurement {
    assert!(batches > 0 && iterations % batches == 0);
    for _ in 0..warmup {
        operation();
    }

    let iterations_per_batch = iterations / batches;
    let mut batch_means = Vec::with_capacity(batches as usize);
    let allocations_before = allocation::snapshot();
    for _ in 0..batches {
        let started = std::time::Instant::now();
        for _ in 0..iterations_per_batch {
            operation();
        }
        batch_means.push(
            started.elapsed().as_secs_f64() * 1_000_000_000.0 / f64::from(iterations_per_batch),
        );
    }
    let allocations = allocation::snapshot().difference(allocations_before);
    EntryMeasurement {
        iterations,
        batches,
        iterations_per_batch,
        batch_means,
        allocations,
    }
}

#[cfg(target_os = "macos")]
struct EntryMeasurement {
    iterations: u32,
    batches: u32,
    iterations_per_batch: u32,
    batch_means: Vec<f64>,
    allocations: allocation::Snapshot,
}

#[cfg(target_os = "macos")]
impl EntryMeasurement {
    fn mean(&self) -> f64 {
        self.batch_means.iter().sum::<f64>() / f64::from(self.batches)
    }
}

#[cfg(target_os = "macos")]
fn print_entry_measurement(name: &str, measurement: &EntryMeasurement) {
    use std::fmt::Write;

    println!("{name}: {:.2} ns/entry", measurement.mean());

    let mut samples = String::new();
    for (index, sample) in measurement.batch_means.iter().enumerate() {
        if index > 0 {
            samples.push(',');
        }
        write!(&mut samples, "{sample:.4}").expect("write batch sample");
    }
    println!(
        "entry_batches_{name}: {} ops/batch {samples} ns/entry",
        measurement.iterations_per_batch
    );
    println!(
        "rust_alloc_{name}: {} calls {} bytes {} deallocations {} deallocated-bytes ({} iterations)",
        measurement.allocations.allocations,
        measurement.allocations.allocated_bytes,
        measurement.allocations.deallocations,
        measurement.allocations.deallocated_bytes,
        measurement.iterations,
    );
}

#[cfg(not(target_os = "macos"))]
fn main() {}

#[cfg(target_os = "macos")]
mod allocation {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::sync::atomic::{AtomicU64, Ordering};

    static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);
    static ALLOCATED_BYTES: AtomicU64 = AtomicU64::new(0);
    static DEALLOCATIONS: AtomicU64 = AtomicU64::new(0);
    static DEALLOCATED_BYTES: AtomicU64 = AtomicU64::new(0);

    pub(super) struct CountingAllocator;

    // SAFETY: Every operation delegates to `System` with the original pointer and
    // layout. The counters are observational and do not affect allocator state.
    unsafe impl GlobalAlloc for CountingAllocator {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            // SAFETY: The caller supplies the allocation contract required by
            // `GlobalAlloc`; it is forwarded unchanged to `System`.
            let pointer = unsafe { System.alloc(layout) };
            if !pointer.is_null() {
                record_allocation(layout.size());
            }
            pointer
        }

        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            // SAFETY: The caller supplies the allocation contract required by
            // `GlobalAlloc`; it is forwarded unchanged to `System`.
            let pointer = unsafe { System.alloc_zeroed(layout) };
            if !pointer.is_null() {
                record_allocation(layout.size());
            }
            pointer
        }

        unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
            record_deallocation(layout.size());
            // SAFETY: The pointer and layout come from the caller's matching
            // allocation contract and are forwarded unchanged to `System`.
            unsafe { System.dealloc(pointer, layout) };
        }

        unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
            // SAFETY: The caller supplies a live allocation, its original layout,
            // and the requested new size; all are forwarded to `System`.
            let replacement = unsafe { System.realloc(pointer, layout, size) };
            if !replacement.is_null() {
                record_deallocation(layout.size());
                record_allocation(size);
            }
            replacement
        }
    }

    #[derive(Clone, Copy)]
    pub(super) struct Snapshot {
        pub(super) allocations: u64,
        pub(super) allocated_bytes: u64,
        pub(super) deallocations: u64,
        pub(super) deallocated_bytes: u64,
    }

    impl Snapshot {
        pub(super) fn difference(self, earlier: Self) -> Self {
            Self {
                allocations: self.allocations.wrapping_sub(earlier.allocations),
                allocated_bytes: self.allocated_bytes.wrapping_sub(earlier.allocated_bytes),
                deallocations: self.deallocations.wrapping_sub(earlier.deallocations),
                deallocated_bytes: self
                    .deallocated_bytes
                    .wrapping_sub(earlier.deallocated_bytes),
            }
        }
    }

    pub(super) fn snapshot() -> Snapshot {
        Snapshot {
            allocations: ALLOCATIONS.load(Ordering::Relaxed),
            allocated_bytes: ALLOCATED_BYTES.load(Ordering::Relaxed),
            deallocations: DEALLOCATIONS.load(Ordering::Relaxed),
            deallocated_bytes: DEALLOCATED_BYTES.load(Ordering::Relaxed),
        }
    }

    fn record_allocation(size: usize) {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        ALLOCATED_BYTES.fetch_add(size as u64, Ordering::Relaxed);
    }

    fn record_deallocation(size: usize) {
        DEALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        DEALLOCATED_BYTES.fetch_add(size as u64, Ordering::Relaxed);
    }
}

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

        #[link_name = "JSValueProtect"]
        fn protect(context: Context, value: Value);

        #[link_name = "JSValueUnprotect"]
        fn unprotect(context: Context, value: Value);
    }

    pub(super) struct OwnedContext(GlobalContext);

    impl OwnedContext {
        pub(super) fn new() -> Self {
            // SAFETY: A null class requests the default global. Check before use.
            let context = unsafe { context_create(ptr::null_mut()) };
            assert!(!context.is_null(), "create direct JSC context");
            Self(context)
        }

        pub(super) fn as_void(&self) -> *mut c_void {
            self.0.cast()
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

    pub(super) fn measure(warmup: u32, iterations: u32) -> f64 {
        let owner = OwnedContext::new();
        let rooted = RootedFunction::new(&owner);
        let context = owner.0;
        let function = rooted.function;
        // SAFETY: Both primitive values are created in the live context.
        let arguments = unsafe { [make_number(context, 20.0), make_number(context, 22.0)] };
        check_result(context, call(context, function, &arguments));

        for _ in 0..warmup {
            black_box(call(context, function, &arguments));
        }
        let started = Instant::now();
        for _ in 0..iterations {
            black_box(call(context, function, &arguments));
        }
        let elapsed = started.elapsed();

        check_result(context, call(context, function, &arguments));
        elapsed.as_secs_f64() * 1_000_000_000.0 / f64::from(iterations)
    }

    pub(super) fn measure_scalar(warmup: u32, iterations: u32) -> f64 {
        let owner = OwnedContext::new();
        let context = owner.0;
        super::assert_answer(scalar_round_trip(context, 42.0));

        for _ in 0..warmup {
            black_box(scalar_round_trip(context, black_box(42.0)));
        }
        let started = Instant::now();
        for _ in 0..iterations {
            black_box(scalar_round_trip(context, black_box(42.0)));
        }
        let elapsed = started.elapsed();

        super::assert_answer(scalar_round_trip(context, 42.0));
        elapsed.as_secs_f64() * 1_000_000_000.0 / f64::from(iterations)
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

    fn scalar_round_trip(context: Context, number: f64) -> f64 {
        // SAFETY: The context remains live throughout this primitive round-trip.
        let value = unsafe { make_number(context, number) };
        // SAFETY: The value was created in this context immediately above.
        assert!(unsafe { is_number(context, value) });
        let mut exception = ptr::null();
        // SAFETY: Strict type checking avoids coercion and captures exceptions.
        let result = unsafe { to_number(context, value, &raw mut exception) };
        assert!(exception.is_null(), "direct scalar conversion threw");
        result
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
