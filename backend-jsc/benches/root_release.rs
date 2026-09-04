// SPDX-License-Identifier: MIT OR Apache-2.0

//! Last-lease drop and host-entry release drain, timed separately.

#[cfg(target_os = "macos")]
fn main() {
    use rustjsi_backend_jsc::{Persistent, Runtime};
    use std::hint::black_box;
    use std::time::{Duration, Instant};

    const WARMUP: u32 = 5;
    const ROUNDS: u32 = 100;

    fn roots(runtime: &mut Runtime, count: usize) -> Vec<Persistent> {
        runtime
            .with_context(|cx| {
                let value = cx.eval("({})", "release-bench.js").unwrap();
                (0..count).map(|_| cx.persist(&value).unwrap()).collect()
            })
            .unwrap()
    }

    for live in [0, 4096] {
        for pending in [0, 1, 64, 1024, 16_384] {
            let mut runtime = Runtime::new().unwrap();
            let unrelated = roots(&mut runtime, live);
            let mut drop_total = Duration::ZERO;
            let mut drain_total = Duration::ZERO;
            for round in 0..WARMUP + ROUNDS {
                // Allocation, evaluation and protection are outside both timers.
                // Separate registry roots share one JS object to isolate release
                // bookkeeping from per-object allocation and collection costs.
                let batch = black_box(roots(&mut runtime, pending));
                let start = Instant::now();
                drop(batch);
                let drop_elapsed = start.elapsed();
                let start = Instant::now();
                runtime.with_backend(|_| black_box(())).unwrap();
                let drain_elapsed = start.elapsed();
                if round >= WARMUP {
                    drop_total += drop_elapsed;
                    drain_total += drain_elapsed;
                }
            }
            println!(
                "live={live} pending={pending} drop_ns/batch={:.2} drain_ns/entry={:.2} rounds={ROUNDS}",
                drop_total.as_secs_f64() * 1e9 / f64::from(ROUNDS),
                drain_total.as_secs_f64() * 1e9 / f64::from(ROUNDS),
            );
            drop(unrelated);
            runtime.invalidate().unwrap();
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn main() {}
