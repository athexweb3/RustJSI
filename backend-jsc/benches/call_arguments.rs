// SPDX-License-Identifier: MIT OR Apache-2.0

//! Call preparation, temporary roots and result capture for varying string arity.

#[cfg(target_os = "macos")]
fn main() {
    use rustjsi_backend_jsc::{Runtime, Value};
    use std::hint::black_box;
    use std::time::Instant;

    for count in [0, 1, 8, 9, 128] {
        let arguments: Vec<_> = (0..count)
            .map(|i| Value::String(format!("{i}:{}", "x".repeat(32))))
            .collect();
        let mut runtime = Runtime::new().unwrap();
        runtime
            .with_context(|cx| {
                let function = cx
                    .install_host_function("accept", |call| {
                        black_box(call.len());
                        Ok(Value::Undefined)
                    })
                    .unwrap();
                for _ in 0..1000 {
                    black_box(cx.call(&function, &arguments).unwrap());
                }
                let iterations = 10_000_u32;
                for sample in 1..=3 {
                    let start = Instant::now();
                    for _ in 0..iterations {
                        black_box(cx.call(&function, black_box(&arguments)).unwrap());
                    }
                    println!(
                        "strings={count} sample={sample} ns/call={:.2}",
                        start.elapsed().as_secs_f64() * 1e9 / f64::from(iterations)
                    );
                }
            })
            .unwrap();
    }
}

#[cfg(not(target_os = "macos"))]
fn main() {}
