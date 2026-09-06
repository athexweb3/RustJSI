// SPDX-License-Identifier: MIT OR Apache-2.0

//! Direct JSC host-function boundary microbenchmark.

#[cfg(target_os = "macos")]
#[path = "support/callbacks.rs"]
mod callbacks;

#[cfg(target_os = "macos")]
fn main() {
    use rustjsi_backend::{BackendBase, BackendScope};
    use rustjsi_backend_jsc::{Attachment, Runtime};
    use rustjsi_host::{EntryGate, FinalEntryPolicy, RuntimeIdentity};
    use std::hint::black_box;
    use std::num::NonZeroU32;
    use std::time::Instant;

    const WARMUP: u32 = 10_000;
    const ITERATIONS: u32 = 1_000_000;
    const ENTRY_BATCHES: u32 = 1_000;

    let callback_order = callbacks::selected_order();
    let calls = measure_callback_workloads(callback_order, WARMUP, ITERATIONS);
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

    print_call_measurements(callback_order, calls, ITERATIONS);
    print_entry_measurement("host_gate_admit_and_exit", &gate_entry);
    print_entry_measurement("jsc_common_empty_entry", &common_entry);
    print_entry_measurement("jsc_foreign_common_empty_entry", &foreign_common_entry);
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
#[derive(Clone, Copy)]
struct CallbackMeasurements {
    lower_bound: f64,
    prepared: f64,
    rustjsi: f64,
}

#[cfg(target_os = "macos")]
fn measure_callback_workloads(
    order: [callbacks::CallbackWorkload; 3],
    warmup: u32,
    iterations: u32,
) -> CallbackMeasurements {
    let mut lower_bound = None;
    let mut prepared = None;
    let mut rustjsi = None;
    for workload in order {
        match workload {
            callbacks::CallbackWorkload::Reused => {
                lower_bound = Some(callbacks::with_operation(workload, |operation| {
                    measure_callback(warmup, iterations, operation)
                }));
            }
            callbacks::CallbackWorkload::Prepared => {
                prepared = Some(callbacks::with_operation(workload, |operation| {
                    measure_callback(warmup, iterations, operation)
                }));
            }
            callbacks::CallbackWorkload::RustJsi => {
                rustjsi = Some(callbacks::with_operation(workload, |operation| {
                    measure_callback(warmup, iterations, operation)
                }));
            }
        }
    }
    CallbackMeasurements {
        lower_bound: lower_bound.expect("measure reused-argument callback"),
        prepared: prepared.expect("measure prepared-argument callback"),
        rustjsi: rustjsi.expect("measure RustJSI callback"),
    }
}

#[cfg(target_os = "macos")]
fn measure_callback(warmup: u32, iterations: u32, operation: &mut dyn FnMut()) -> f64 {
    for _ in 0..warmup {
        operation();
    }
    let started = std::time::Instant::now();
    for _ in 0..iterations {
        operation();
    }
    started.elapsed().as_secs_f64() * 1_000_000_000.0 / f64::from(iterations)
}

#[cfg(target_os = "macos")]
fn print_call_measurements(
    order: [callbacks::CallbackWorkload; 3],
    measurements: CallbackMeasurements,
    iterations: u32,
) {
    let [first, second, third] = order.map(callbacks::CallbackWorkload::name);
    println!("callback_order: {first},{second},{third}");
    println!(
        "direct_jsc_lower_bound: {:.2} ns/call",
        measurements.lower_bound
    );
    println!(
        "direct_jsc_prepared_call: {:.2} ns/call",
        measurements.prepared
    );
    println!("rustjsi_experimental: {:.2} ns/call", measurements.rustjsi);
    println!(
        "rustjsi_over_direct: {:.3}x ({iterations} iterations)",
        measurements.rustjsi / measurements.lower_bound
    );
    println!(
        "rustjsi_over_prepared: {:.3}x ({iterations} iterations)",
        measurements.rustjsi / measurements.prepared
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
    for _ in 0..batches {
        let started = std::time::Instant::now();
        for _ in 0..iterations_per_batch {
            operation();
        }
        batch_means.push(
            started.elapsed().as_secs_f64() * 1_000_000_000.0 / f64::from(iterations_per_batch),
        );
    }
    EntryMeasurement {
        batches,
        iterations_per_batch,
        batch_means,
    }
}

#[cfg(target_os = "macos")]
struct EntryMeasurement {
    batches: u32,
    iterations_per_batch: u32,
    batch_means: Vec<f64>,
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

    #[link(name = "JavaScriptCore", kind = "framework")]
    unsafe extern "C" {
        #[link_name = "JSGlobalContextCreate"]
        fn context_create(class: *mut c_void) -> GlobalContext;

        #[link_name = "JSGlobalContextRelease"]
        fn context_release(context: GlobalContext);

        #[link_name = "JSValueMakeNumber"]
        fn make_number(context: Context, number: f64) -> Value;

        #[link_name = "JSValueIsNumber"]
        fn is_number(context: Context, value: Value) -> bool;

        #[link_name = "JSValueToNumber"]
        fn to_number(context: Context, value: Value, exception: *mut Value) -> f64;

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
}
