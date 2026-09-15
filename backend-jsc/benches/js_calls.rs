// SPDX-License-Identifier: MIT OR Apache-2.0

//! Prepared scalar calls to the same JavaScript function through two APIs.

#[cfg(target_os = "macos")]
#[path = "support/js_calls.rs"]
mod workloads;

#[cfg(target_os = "macos")]
fn main() {
    use std::hint::black_box;
    use std::time::Instant;

    const ITERATIONS: u32 = 1_000_000;

    fn measure(name: &str, operation: &mut dyn FnMut()) {
        for _ in 0..100_000 {
            operation();
        }
        let start = Instant::now();
        for _ in 0..ITERATIONS {
            operation();
        }
        let elapsed = start.elapsed();
        black_box(operation);
        println!(
            "{name}: {:.3} ns/call ({ITERATIONS} iterations)",
            elapsed.as_secs_f64() * 1e9 / f64::from(ITERATIONS)
        );
    }

    for workload in workloads::selected_order() {
        let name = workload.labels().1;
        workloads::with_operation(workload, |operation| measure(name, operation));
    }
}

#[cfg(not(target_os = "macos"))]
fn main() {}
