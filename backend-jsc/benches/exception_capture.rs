// SPDX-License-Identifier: MIT OR Apache-2.0

//! Exception capture for pre-created messages; includes evaluation and error drop.

#[cfg(target_os = "macos")]
fn main() {
    use rustjsi_backend_jsc::{Context, JsError, Runtime};
    use std::hint::black_box;
    use std::time::Instant;

    fn capture(cx: &mut Context<'_>) -> usize {
        match black_box(cx.eval("throw failure", "capture.js")) {
            Err(JsError::Exception(error)) => black_box(error.message().len()),
            _ => panic!("expected JavaScript exception"),
        }
    }

    for bytes in [32, 4096, 1_048_576] {
        let mut runtime = Runtime::new().unwrap();
        runtime
            .with_context(|cx| {
                cx.eval(
                    &format!("globalThis.failure = 'x'.repeat({bytes}); undefined"),
                    "setup.js",
                )
                .unwrap();
                for _ in 0..100 {
                    capture(cx);
                }
                let iterations = 1000_u32;
                for sample in 1..=3 {
                    let mut captured_bytes = 0;
                    let start = Instant::now();
                    for _ in 0..iterations {
                        captured_bytes = capture(black_box(&mut *cx));
                    }
                    println!(
                        "input_bytes={bytes} sample={sample} captured_bytes={captured_bytes} ns/capture={:.2}",
                        start.elapsed().as_secs_f64() * 1e9 / f64::from(iterations)
                    );
                }
            })
            .unwrap();
    }
}

#[cfg(not(target_os = "macos"))]
fn main() {}
