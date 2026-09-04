// SPDX-License-Identifier: MIT OR Apache-2.0

//! Synchronous typed native-state access within an admitted host entry.

#[cfg(target_os = "macos")]
fn main() {
    use rustjsi_backend_jsc::Runtime;
    use std::cell::Cell;
    use std::hint::black_box;
    use std::time::Instant;

    const WARMUP: u32 = 10_000;
    const ITERATIONS: u32 = 1_000_000;

    let mut runtime = Runtime::new().unwrap();
    runtime
        .with_context(|cx| {
            let handle = cx
                .install_native_state("resource", Cell::new(42_u64))
                .unwrap();
            for _ in 0..WARMUP {
                black_box(&mut *cx)
                    .with_native_state(black_box(&handle), |state| black_box(state.get()))
                    .unwrap();
            }
            let start = Instant::now();
            for _ in 0..ITERATIONS {
                black_box(&mut *cx)
                    .with_native_state(black_box(&handle), |state| black_box(state.get()))
                    .unwrap();
            }
            let ns = start.elapsed().as_secs_f64() * 1e9 / f64::from(ITERATIONS);
            println!("native_state_read: {ns:.2} ns/access ({ITERATIONS} iterations)");
        })
        .unwrap();
}

#[cfg(not(target_os = "macos"))]
fn main() {}
