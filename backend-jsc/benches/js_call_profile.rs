// SPDX-License-Identifier: MIT OR Apache-2.0

//! Long-running scoped JavaScript-call workload for sampling profilers.

#[cfg(target_os = "macos")]
#[path = "support/js_calls.rs"]
mod js_calls;

#[cfg(target_os = "macos")]
fn main() {
    use std::time::Instant;

    const WARMUP: u32 = 100_000;
    const DEFAULT_ITERATIONS: u32 = 50_000_000;

    let selected = std::env::var("RUSTJSI_JS_CALL_PROFILE")
        .expect("set RUSTJSI_JS_CALL_PROFILE to direct or common");
    let workload = match selected.as_str() {
        "direct" => js_calls::JsCallWorkload::Direct,
        "common" => js_calls::JsCallWorkload::Common,
        _ => panic!("invalid RUSTJSI_JS_CALL_PROFILE: {selected}"),
    };
    let iterations =
        std::env::var("RUSTJSI_PROFILE_ITERATIONS").map_or(DEFAULT_ITERATIONS, |value| {
            value
                .parse::<u32>()
                .ok()
                .filter(|iterations| *iterations > 0)
                .expect("RUSTJSI_PROFILE_ITERATIONS must be a positive u32")
        });
    let label = workload.labels().0;
    js_calls::with_operation(workload, |operation| {
        for _ in 0..WARMUP {
            operation();
        }
        let started = Instant::now();
        for _ in 0..iterations {
            operation();
        }
        let elapsed = started.elapsed().as_secs_f64();
        println!(
            "js_call_profile: {label} {iterations} calls {elapsed:.6} seconds {:.3} ns/call",
            elapsed * 1_000_000_000.0 / f64::from(iterations)
        );
    });
}

#[cfg(not(target_os = "macos"))]
fn main() {}
