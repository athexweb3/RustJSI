// SPDX-License-Identifier: MIT OR Apache-2.0

//! Registration, full-registry rejection and teardown with empty captures.

#[cfg(target_os = "macos")]
fn main() {
    use rustjsi_backend_jsc::{JsError, Runtime, RuntimeError, Value};
    use std::hint::black_box;
    use std::time::Instant;

    const REGISTRATIONS: u32 = 4096;
    const REJECTIONS: u32 = 1_000_000;

    let mut warmup = Runtime::new().unwrap();
    warmup
        .with_context(|cx| {
            for _ in 0..REGISTRATIONS {
                black_box(
                    cx.install_host_function("retained", |_| Ok(Value::Undefined))
                        .unwrap(),
                );
            }
            for _ in 0..10_000 {
                assert!(
                    cx.install_host_function("overflow", |_| Ok(Value::Undefined))
                        .is_err()
                );
            }
        })
        .unwrap();
    drop(warmup);

    for sample in 1..=3 {
        let mut runtime = Runtime::new().unwrap();
        runtime
            .with_context(|cx| {
                let start = Instant::now();
                for _ in 0..REGISTRATIONS {
                    black_box(
                        black_box(&mut *cx)
                            .install_host_function("retained", |_| Ok(Value::Undefined))
                            .unwrap(),
                    );
                }
                println!(
                    "sample={sample} register_ns={:.2}",
                    start.elapsed().as_secs_f64() * 1e9 / f64::from(REGISTRATIONS)
                );
                let start = Instant::now();
                for _ in 0..REJECTIONS {
                    assert!(matches!(
                        black_box(
                            black_box(&mut *cx)
                                .install_host_function("overflow", |_| Ok(Value::Undefined))
                        ),
                        Err(JsError::Runtime(RuntimeError::HostFunctionLimitReached))
                    ));
                }
                println!(
                    "sample={sample} reject_ns={:.2}",
                    start.elapsed().as_secs_f64() * 1e9 / f64::from(REJECTIONS)
                );
            })
            .unwrap();
        let start = Instant::now();
        runtime.invalidate().unwrap();
        println!(
            "sample={sample} invalidate_4096_us={:.2}",
            start.elapsed().as_secs_f64() * 1e6
        );
    }
}

#[cfg(not(target_os = "macos"))]
fn main() {}
