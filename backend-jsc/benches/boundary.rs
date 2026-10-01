// SPDX-License-Identifier: MIT OR Apache-2.0

//! Direct JSC host-function boundary microbenchmark.

#[cfg(target_os = "macos")]
#[path = "support/callbacks.rs"]
mod callbacks;

#[cfg(target_os = "macos")]
#[path = "support/js_calls.rs"]
mod js_calls;

#[cfg(target_os = "macos")]
#[path = "support/entries.rs"]
mod entries;

#[cfg(target_os = "macos")]
#[path = "support/scalars.rs"]
mod scalars;

#[cfg(target_os = "macos")]
fn main() {
    use std::hint::black_box;

    const WARMUP: u32 = 10_000;
    const ITERATIONS: u32 = 1_000_000;
    const CALLBACK_BATCHES: u32 = 1_000;
    const ENTRY_BATCHES: u32 = 1_000;

    let callback_order = callbacks::selected_order();
    let calls = measure_callback_workloads(callback_order, WARMUP, ITERATIONS, CALLBACK_BATCHES);
    let js_call_order = js_calls::selected_order();
    let js_calls = measure_js_call_workloads(js_call_order, WARMUP, ITERATIONS, CALLBACK_BATCHES);
    let entry_order = entries::selected_order();
    let entry = measure_entry_workloads(entry_order, WARMUP, ITERATIONS, ENTRY_BATCHES);
    let scalar_order = scalars::selected_order();
    let scalar = measure_scalar_workloads(scalar_order, WARMUP, ITERATIONS);

    let timer_pairs = measure_timer_pairs(ENTRY_BATCHES);
    let empty_batches = measure_batches(WARMUP, ITERATIONS, ENTRY_BATCHES, &mut || {
        black_box(());
    });

    print_call_measurements(callback_order, &calls, ITERATIONS);
    print_js_call_measurements(js_call_order, &js_calls, ITERATIONS);
    print_entry_measurements(entry_order, &entry);
    print_scalar_measurements(scalar_order, &scalar, ITERATIONS);
    print_samples(
        "calibration_timer_pair",
        "samples",
        ENTRY_BATCHES,
        "ns/pair",
        &timer_pairs,
    );
    print_batch_samples("calibration", "empty_batch", "ns/operation", &empty_batches);
}

#[cfg(target_os = "macos")]
struct EntryMeasurements {
    gate: BatchMeasurement,
    common: BatchMeasurement,
    foreign: BatchMeasurement,
}

#[cfg(target_os = "macos")]
fn measure_entry_workloads(
    order: [entries::EntryWorkload; 3],
    warmup: u32,
    iterations: u32,
    batches: u32,
) -> EntryMeasurements {
    use rustjsi_backend_jsc::{Attachment, Runtime};
    use rustjsi_host::{EntryGate, FinalEntryPolicy, RuntimeIdentity};
    use std::hint::black_box;
    use std::num::NonZeroU32;

    let gate = EntryGate::new(
        NonZeroU32::new(64).expect("nonzero entry limit"),
        FinalEntryPolicy::Unavailable,
    );
    let mut runtime = Runtime::new().expect("create RustJSI JSC runtime");
    let foreign_owner = raw::OwnedContext::new();
    let mut identity = RuntimeIdentity::allocate().expect("allocate foreign host identity");
    let mut attachment = Attachment::new(&mut identity, FinalEntryPolicy::Guaranteed)
        .expect("create foreign attachment");
    let mut gate_measurement = None;
    let mut common_measurement = None;
    let mut foreign_measurement = None;

    for workload in order {
        let measurement = match workload {
            entries::EntryWorkload::Gate => {
                measure_batches(warmup, iterations, batches, &mut || {
                    let entry = black_box(&gate).try_enter().expect("admit host entry");
                    black_box(&entry);
                    drop(entry);
                })
            }
            entries::EntryWorkload::Common => {
                measure_batches(warmup, iterations, batches, &mut || {
                    black_box(&mut runtime)
                        .with_backend(|_| black_box(()))
                        .expect("enter common backend");
                })
            }
            entries::EntryWorkload::Foreign => {
                measure_batches(warmup, iterations, batches, &mut || {
                    // SAFETY: The benchmark owner keeps this context live on the
                    // current thread and lends the same global context to every entry.
                    unsafe {
                        black_box(&mut attachment)
                            .with_backend(foreign_owner.as_void(), |_| black_box(()))
                            .expect("enter foreign common backend");
                    }
                })
            }
        };
        match workload {
            entries::EntryWorkload::Gate => gate_measurement = Some(measurement),
            entries::EntryWorkload::Common => common_measurement = Some(measurement),
            entries::EntryWorkload::Foreign => foreign_measurement = Some(measurement),
        }
    }

    // SAFETY: This is the same still-live context used for every measured entry.
    let _ = unsafe { attachment.detach_with_context(foreign_owner.as_void()) }
        .expect("detach foreign benchmark attachment");
    EntryMeasurements {
        gate: gate_measurement.expect("measure host gate entry"),
        common: common_measurement.expect("measure common entry"),
        foreign: foreign_measurement.expect("measure foreign common entry"),
    }
}

#[cfg(target_os = "macos")]
fn print_entry_measurements(order: [entries::EntryWorkload; 3], measurements: &EntryMeasurements) {
    let [first, second, third] = order.map(|workload| workload.labels().0);
    println!("entry_order: {first},{second},{third}");
    print_entry_measurement("host_gate_admit_and_exit", &measurements.gate);
    print_entry_measurement("jsc_common_empty_entry", &measurements.common);
    print_entry_measurement("jsc_foreign_common_empty_entry", &measurements.foreign);
}

#[cfg(target_os = "macos")]
struct ScalarMeasurements {
    direct: f64,
    common: f64,
}

#[cfg(target_os = "macos")]
fn measure_scalar_workloads(
    order: [scalars::ScalarWorkload; 2],
    warmup: u32,
    iterations: u32,
) -> ScalarMeasurements {
    let mut direct = None;
    let mut common = None;
    for workload in order {
        let measurement = match workload {
            scalars::ScalarWorkload::Direct => raw::measure_scalar(warmup, iterations),
            scalars::ScalarWorkload::Common => measure_common_scalar(warmup, iterations),
        };
        match workload {
            scalars::ScalarWorkload::Direct => direct = Some(measurement),
            scalars::ScalarWorkload::Common => common = Some(measurement),
        }
    }
    ScalarMeasurements {
        direct: direct.expect("measure direct scalar round-trip"),
        common: common.expect("measure common scalar round-trip"),
    }
}

#[cfg(target_os = "macos")]
fn measure_common_scalar(warmup: u32, iterations: u32) -> f64 {
    use rustjsi_backend::{BackendBase, BackendScope};
    use rustjsi_backend_jsc::Runtime;
    use std::hint::black_box;
    use std::time::Instant;

    let mut runtime = Runtime::new().expect("create common scalar runtime");
    runtime
        .with_backend(|backend| {
            let scope = backend.open_scope().expect("open common JSC scope");
            let value = scope.number(42.0).expect("preflight number");
            assert_answer(scope.as_number(value).expect("read preflight number"));
            for _ in 0..warmup {
                let value = scope.number(black_box(42.0)).expect("make number");
                black_box(scope.as_number(value).expect("read number"));
            }
            let started = Instant::now();
            for _ in 0..iterations {
                let value = scope.number(black_box(42.0)).expect("make number");
                black_box(scope.as_number(value).expect("read number"));
            }
            let elapsed = started.elapsed();
            let value = scope.number(42.0).expect("postflight number");
            assert_answer(scope.as_number(value).expect("read postflight number"));
            elapsed.as_secs_f64() * 1_000_000_000.0 / f64::from(iterations)
        })
        .expect("enter common JSC backend")
}

#[cfg(target_os = "macos")]
fn print_scalar_measurements(
    order: [scalars::ScalarWorkload; 2],
    measurements: &ScalarMeasurements,
    iterations: u32,
) {
    let [first, second] = order.map(|workload| workload.labels().0);
    println!("scalar_order: {first},{second}");
    println!(
        "direct_jsc_scalar: {:.2} ns/round-trip",
        measurements.direct
    );
    println!(
        "rustjsi_common_scalar: {:.2} ns/round-trip",
        measurements.common
    );
    println!(
        "common_scalar_over_direct: {:.3}x ({iterations} iterations)",
        measurements.common / measurements.direct
    );
}

#[cfg(target_os = "macos")]
struct JsCallMeasurements {
    direct: BatchMeasurement,
    common: BatchMeasurement,
}

#[cfg(target_os = "macos")]
fn measure_js_call_workloads(
    order: [js_calls::JsCallWorkload; 2],
    warmup: u32,
    iterations: u32,
    batches: u32,
) -> JsCallMeasurements {
    let mut direct = None;
    let mut common = None;
    for workload in order {
        let measurement = js_calls::with_operation(workload, |operation| {
            measure_batches(warmup, iterations, batches, operation)
        });
        match workload {
            js_calls::JsCallWorkload::Direct => direct = Some(measurement),
            js_calls::JsCallWorkload::Common => common = Some(measurement),
        }
    }
    JsCallMeasurements {
        direct: direct.expect("measure direct JavaScript calls"),
        common: common.expect("measure common JavaScript calls"),
    }
}

#[cfg(target_os = "macos")]
fn print_js_call_measurements(
    order: [js_calls::JsCallWorkload; 2],
    measurements: &JsCallMeasurements,
    iterations: u32,
) {
    let [first, second] = order.map(|workload| workload.labels().0);
    let direct = measurements.direct.mean();
    let common = measurements.common.mean();
    println!("js_call_order: {first},{second}");
    println!("direct_jsc_js_call: {direct:.2} ns/call");
    println!("rustjsi_common_js_call: {common:.2} ns/call");
    println!(
        "common_js_call_over_direct: {:.3}x ({iterations} iterations)",
        common / direct
    );
    print_batch_samples(
        "js_call_batches",
        "direct_jsc_js_call",
        "ns/call",
        &measurements.direct,
    );
    print_batch_samples(
        "js_call_batches",
        "rustjsi_common_js_call",
        "ns/call",
        &measurements.common,
    );
}

#[cfg(target_os = "macos")]
fn assert_answer(value: f64) {
    assert!(
        (value - 42.0).abs() < f64::EPSILON,
        "wrong benchmark result: {value}"
    );
}

#[cfg(target_os = "macos")]
struct CallbackMeasurements {
    lower_bound: BatchMeasurement,
    prepared: BatchMeasurement,
    rustjsi: BatchMeasurement,
}

#[cfg(target_os = "macos")]
fn measure_callback_workloads(
    order: [callbacks::CallbackWorkload; 3],
    warmup: u32,
    iterations: u32,
    batches: u32,
) -> CallbackMeasurements {
    let mut lower_bound = None;
    let mut prepared = None;
    let mut rustjsi = None;
    for workload in order {
        match workload {
            callbacks::CallbackWorkload::Reused => {
                lower_bound = Some(callbacks::with_operation(workload, |operation| {
                    measure_batches(warmup, iterations, batches, operation)
                }));
            }
            callbacks::CallbackWorkload::Prepared => {
                prepared = Some(callbacks::with_operation(workload, |operation| {
                    measure_batches(warmup, iterations, batches, operation)
                }));
            }
            callbacks::CallbackWorkload::RustJsi => {
                rustjsi = Some(callbacks::with_operation(workload, |operation| {
                    measure_batches(warmup, iterations, batches, operation)
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
fn print_call_measurements(
    order: [callbacks::CallbackWorkload; 3],
    measurements: &CallbackMeasurements,
    iterations: u32,
) {
    let [first, second, third] = order.map(|workload| workload.labels().0);
    let lower_bound = measurements.lower_bound.mean();
    let prepared = measurements.prepared.mean();
    let rustjsi = measurements.rustjsi.mean();
    println!("callback_order: {first},{second},{third}");
    println!("direct_jsc_lower_bound: {lower_bound:.2} ns/call");
    println!("direct_jsc_prepared_call: {prepared:.2} ns/call");
    println!("rustjsi_experimental: {rustjsi:.2} ns/call");
    println!(
        "rustjsi_over_direct: {:.3}x ({iterations} iterations)",
        rustjsi / lower_bound
    );
    println!(
        "rustjsi_over_prepared: {:.3}x ({iterations} iterations)",
        rustjsi / prepared
    );
    print_batch_samples(
        "callback_batches",
        "direct_jsc_lower_bound",
        "ns/call",
        &measurements.lower_bound,
    );
    print_batch_samples(
        "callback_batches",
        "direct_jsc_prepared_call",
        "ns/call",
        &measurements.prepared,
    );
    print_batch_samples(
        "callback_batches",
        "rustjsi_experimental",
        "ns/call",
        &measurements.rustjsi,
    );
}

#[cfg(target_os = "macos")]
fn measure_batches(
    warmup: u32,
    iterations: u32,
    batches: u32,
    operation: &mut dyn FnMut(),
) -> BatchMeasurement {
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
    BatchMeasurement {
        batches,
        iterations_per_batch,
        batch_means,
    }
}

#[cfg(target_os = "macos")]
fn measure_timer_pairs(samples: u32) -> Vec<f64> {
    let mut elapsed = Vec::with_capacity(samples as usize);
    for _ in 0..samples {
        let started = std::time::Instant::now();
        elapsed.push(started.elapsed().as_secs_f64() * 1_000_000_000.0);
    }
    elapsed
}

#[cfg(target_os = "macos")]
struct BatchMeasurement {
    batches: u32,
    iterations_per_batch: u32,
    batch_means: Vec<f64>,
}

#[cfg(target_os = "macos")]
impl BatchMeasurement {
    fn mean(&self) -> f64 {
        self.batch_means.iter().sum::<f64>() / f64::from(self.batches)
    }
}

#[cfg(target_os = "macos")]
fn print_entry_measurement(name: &str, measurement: &BatchMeasurement) {
    println!("{name}: {:.2} ns/entry", measurement.mean());
    print_batch_samples("entry_batches", name, "ns/entry", measurement);
}

#[cfg(target_os = "macos")]
fn print_batch_samples(prefix: &str, name: &str, unit: &str, measurement: &BatchMeasurement) {
    print_samples(
        &format!("{prefix}_{name}"),
        "ops/batch",
        measurement.iterations_per_batch,
        unit,
        &measurement.batch_means,
    );
}

#[cfg(target_os = "macos")]
fn print_samples(name: &str, count_unit: &str, count: u32, unit: &str, values: &[f64]) {
    use std::fmt::Write;

    let mut samples = String::new();
    for (index, sample) in values.iter().enumerate() {
        if index > 0 {
            samples.push(',');
        }
        write!(&mut samples, "{sample:.4}").expect("write batch sample");
    }
    println!("{name}: {count} {count_unit} {samples} {unit}");
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
