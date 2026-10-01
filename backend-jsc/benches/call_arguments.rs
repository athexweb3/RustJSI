// SPDX-License-Identifier: MIT OR Apache-2.0

//! Call-data admission and temporary argument preparation probes.

#[cfg(target_os = "macos")]
fn main() {
    use rustjsi_backend_jsc::{CallLimits, RuntimeError, Value};

    const WARMUP: u32 = 1_000;
    const ITERATIONS: u32 = 10_000;
    const SAMPLES: u32 = 3;

    for count in [0, 1, 8, 9, 128] {
        let arguments: Vec<_> = (0..count)
            .map(|index| Value::String(format!("{index}:{}", "x".repeat(32))))
            .collect();
        measure_admitted(&arguments, WARMUP, ITERATIONS, SAMPLES);
    }

    let defaults = CallLimits::default();
    let rejected_argument_count = vec![Value::Number(0.0); defaults.arguments + 1];
    measure_rejected(
        "argument_count",
        CallLimits {
            arguments: defaults.arguments,
            string_utf8_bytes: 0,
        },
        &rejected_argument_count,
        RuntimeError::CallArgumentLimitReached,
        WARMUP,
        ITERATIONS,
        SAMPLES,
    );

    let rejected_string_bytes = vec![Value::String(
        "x".repeat(
            defaults
                .string_utf8_bytes
                .checked_add(1)
                .expect("default string limit can exceed one byte"),
        ),
    )];
    measure_rejected(
        "string_utf8_bytes",
        CallLimits {
            arguments: 1,
            string_utf8_bytes: defaults.string_utf8_bytes,
        },
        &rejected_string_bytes,
        RuntimeError::CallStringDataLimitReached,
        WARMUP,
        ITERATIONS,
        SAMPLES,
    );
}

#[cfg(target_os = "macos")]
fn measure_admitted(
    arguments: &[rustjsi_backend_jsc::Value],
    warmup: u32,
    iterations: u32,
    samples: u32,
) {
    use rustjsi_backend_jsc::{Runtime, Value};
    use std::cell::Cell;
    use std::hint::black_box;
    use std::rc::Rc;
    use std::time::Instant;

    let string_utf8_bytes = string_utf8_bytes(arguments);
    let mut runtime = Runtime::new().expect("create admitted call runtime");
    runtime
        .with_context(|cx| {
            let calls = Rc::new(Cell::new(0_u64));
            let observed_calls = Rc::clone(&calls);
            let function = cx
                .install_host_function("accept", move |call| {
                    observed_calls.set(observed_calls.get() + 1);
                    black_box(call.len());
                    Ok(Value::Undefined)
                })
                .expect("install admitted call target");

            for _ in 0..warmup {
                black_box(cx.call(&function, black_box(arguments)).expect("admit warmup call"));
            }
            for sample in 1..=samples {
                let start = Instant::now();
                for _ in 0..iterations {
                    black_box(cx.call(&function, black_box(arguments)).expect("admit timed call"));
                }
                println!(
                    "admit strings={} string_utf8_bytes={string_utf8_bytes} sample={sample} ns/call={:.2}",
                    string_count(arguments),
                    start.elapsed().as_secs_f64() * 1e9 / f64::from(iterations),
                );
            }
            assert_eq!(
                calls.get(),
                u64::from(warmup) + u64::from(iterations) * u64::from(samples),
                "each admitted call reaches its target once",
            );
        })
        .expect("enter admitted call runtime");
}

#[cfg(target_os = "macos")]
#[allow(clippy::too_many_arguments)]
fn measure_rejected(
    rejection: &str,
    limits: rustjsi_backend_jsc::CallLimits,
    arguments: &[rustjsi_backend_jsc::Value],
    expected: rustjsi_backend_jsc::RuntimeError,
    warmup: u32,
    iterations: u32,
    samples: u32,
) {
    use rustjsi_backend_jsc::{JsError, RootLimits, Runtime, Value};
    use std::cell::Cell;
    use std::hint::black_box;
    use std::rc::Rc;
    use std::time::Instant;

    let string_utf8_bytes = string_utf8_bytes(arguments);
    let mut runtime = Runtime::new_with_limits(RootLimits::default(), limits)
        .expect("create rejected call runtime");
    runtime
        .with_context(|cx| {
            let calls = Rc::new(Cell::new(0_u64));
            let observed_calls = Rc::clone(&calls);
            let function = cx
                .install_host_function("reject", move |_| {
                    observed_calls.set(observed_calls.get() + 1);
                    Ok(Value::Undefined)
                })
                .expect("install rejected call target");
            let mut reject = || {
                assert!(matches!(
                    cx.call(&function, black_box(arguments)),
                    Err(JsError::Runtime(error)) if error == expected
                ));
            };

            for _ in 0..warmup {
                reject();
            }
            for sample in 1..=samples {
                let start = Instant::now();
                for _ in 0..iterations {
                    reject();
                }
                println!(
                    "reject={rejection} arguments={} string_utf8_bytes={string_utf8_bytes} sample={sample} ns/call={:.2}",
                    arguments.len(),
                    start.elapsed().as_secs_f64() * 1e9 / f64::from(iterations),
                );
            }
            assert_eq!(calls.get(), 0, "rejected calls never reach their target");
        })
        .expect("enter rejected call runtime");
}

#[cfg(target_os = "macos")]
fn string_count(arguments: &[rustjsi_backend_jsc::Value]) -> usize {
    arguments
        .iter()
        .filter(|argument| matches!(argument, rustjsi_backend_jsc::Value::String(_)))
        .count()
}

#[cfg(target_os = "macos")]
fn string_utf8_bytes(arguments: &[rustjsi_backend_jsc::Value]) -> usize {
    arguments.iter().fold(0, |total, argument| match argument {
        rustjsi_backend_jsc::Value::String(string) => total
            .checked_add(string.len())
            .expect("benchmark string bytes fit usize"),
        _ => total,
    })
}

#[cfg(not(target_os = "macos"))]
fn main() {}
