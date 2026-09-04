// SPDX-License-Identifier: MIT OR Apache-2.0

//! Entry-wide versus batched local-root retention on the same JSC runtime code.

#[cfg(target_os = "macos")]
fn main() {
    use rustjsi_backend_jsc::{Context, Runtime};
    use std::hint::black_box;
    use std::time::Instant;

    const OBJECTS: u32 = 4096;
    const EMPTY_ITERATIONS: u32 = 1_000_000;

    fn evaluate(cx: &mut Context<'_>, count: u32) {
        for _ in 0..count {
            black_box(cx.eval("({})", "local-scopes.js").unwrap());
        }
    }

    let mut runtime = Runtime::new().unwrap();
    runtime
        .with_context(|cx| {
            for _ in 0..10_000 {
                cx.with_scope(|child| {
                    black_box(child);
                })
                .unwrap();
            }
            let start = Instant::now();
            for _ in 0..EMPTY_ITERATIONS {
                cx.with_scope(|child| {
                    black_box(child);
                })
                .unwrap();
            }
            println!(
                "empty_child_scope: {:.2} ns/scope",
                start.elapsed().as_secs_f64() * 1e9 / f64::from(EMPTY_ITERATIONS)
            );
        })
        .unwrap();

    // Each case has a fresh engine and the same warmup. Timers include
    // evaluation, root bookkeeping and local cleanup, but not engine creation.
    for batch in [0, 1, 16, 32, 256] {
        let mut runtime = Runtime::new().unwrap();
        runtime.with_context(|cx| evaluate(cx, 256)).unwrap();
        let start = Instant::now();
        runtime
            .with_context(|cx| {
                if let Some(batches) = OBJECTS.checked_div(batch) {
                    for _ in 0..batches {
                        cx.with_scope(|child| evaluate(child, batch)).unwrap();
                    }
                } else {
                    evaluate(cx, OBJECTS);
                }
            })
            .unwrap();
        println!(
            "objects={OBJECTS} batch={batch} elapsed_us={:.2}",
            start.elapsed().as_secs_f64() * 1e6
        );
    }
}

#[cfg(not(target_os = "macos"))]
fn main() {}
