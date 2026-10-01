// SPDX-License-Identifier: MIT OR Apache-2.0

//! Matched direct and `RustJSI` owned-external-buffer construction profile.

#[cfg(target_os = "macos")]
#[path = "support/external_buffers.rs"]
mod external_buffers;

#[cfg(not(target_os = "macos"))]
fn main() {}

#[cfg(target_os = "macos")]
fn main() {
    const WARMUP_BLOCKS: usize = 12;
    const MEASURED_BLOCKS: usize = 128;

    let payload_bytes =
        std::env::var("RUSTJSI_EXTERNAL_BUFFER_PROFILE_BYTES").map_or(4 * 1024, |value| {
            value
                .parse::<usize>()
                .expect("RUSTJSI_EXTERNAL_BUFFER_PROFILE_BYTES must be a usize")
        });
    let order = selected_order();
    let mut direct = None;
    let mut rustjsi = None;

    for workload in order {
        let samples = match workload {
            Workload::Direct => direct::measure(payload_bytes, WARMUP_BLOCKS, MEASURED_BLOCKS),
            Workload::RustJsi => rustjsi::measure(payload_bytes, WARMUP_BLOCKS, MEASURED_BLOCKS),
        };
        match workload {
            Workload::Direct => direct = Some(samples),
            Workload::RustJsi => rustjsi = Some(samples),
        }
    }

    let direct = direct.expect("measure direct external buffers");
    let rustjsi = rustjsi.expect("measure RustJSI external buffers");
    let direct_mean = mean(&direct);
    let rustjsi_mean = mean(&rustjsi);
    let [first, second] = order.map(Workload::label);

    println!("external_buffer_order: {first},{second}");
    println!("external_buffer_payload_bytes: {payload_bytes}");
    println!("external_buffer_cleanup: property-clear,runtime-teardown");
    print_samples("direct_jsc_external_buffer", &direct);
    print_samples("rustjsi_external_buffer", &rustjsi);
    println!(
        "rustjsi_external_buffer_over_direct: {:.4}x ({MEASURED_BLOCKS} blocks)",
        rustjsi_mean / direct_mean
    );
}

#[cfg(target_os = "macos")]
#[derive(Clone, Copy)]
enum Workload {
    Direct,
    RustJsi,
}

#[cfg(target_os = "macos")]
impl Workload {
    const fn label(self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::RustJsi => "rustjsi",
        }
    }
}

#[cfg(target_os = "macos")]
fn selected_order() -> [Workload; 2] {
    match std::env::var("RUSTJSI_EXTERNAL_BUFFER_PROFILE_ORDER").as_deref() {
        Ok("direct,rustjsi") | Err(_) => [Workload::Direct, Workload::RustJsi],
        Ok("rustjsi,direct") => [Workload::RustJsi, Workload::Direct],
        Ok(value) => panic!("invalid RUSTJSI_EXTERNAL_BUFFER_PROFILE_ORDER: {value}"),
    }
}

#[cfg(target_os = "macos")]
fn print_samples(name: &str, samples: &[f64]) {
    let samples = samples
        .iter()
        .map(|sample| format!("{sample:.4}"))
        .collect::<Vec<_>>()
        .join(",");
    println!(
        "{name}: {} ops/batch {samples} ns/transfer",
        external_buffers::BLOCK_SIZE
    );
}

#[cfg(target_os = "macos")]
fn mean(samples: &[f64]) -> f64 {
    let sample_count = u32::try_from(samples.len()).expect("sample count fits in u32");
    samples.iter().sum::<f64>() / f64::from(sample_count)
}

#[cfg(target_os = "macos")]
mod direct {
    use super::external_buffers::{BLOCK_SIZE, direct as workload, payloads};
    use std::time::Instant;

    pub(super) fn measure(
        payload_bytes: usize,
        warmup_blocks: usize,
        measured_blocks: usize,
    ) -> Vec<f64> {
        for _ in 0..warmup_blocks {
            let _ = measure_block(payload_bytes);
        }
        (0..measured_blocks)
            .map(|_| measure_block(payload_bytes))
            .collect()
    }

    fn measure_block(payload_bytes: usize) -> f64 {
        let mut context = workload::ContextOwner::new();
        let owners = payloads(payload_bytes);
        let started = Instant::now();
        context.publish_block(owners);
        let elapsed = started.elapsed();
        context.clear_properties();
        context.shutdown();
        let block_size = u32::try_from(BLOCK_SIZE).expect("profile block size fits in u32");
        elapsed.as_secs_f64() * 1_000_000_000.0 / f64::from(block_size)
    }
}

#[cfg(target_os = "macos")]
mod rustjsi {
    use super::external_buffers::{BLOCK_SIZE, payloads, rustjsi as workload};
    use rustjsi_backend_jsc::Runtime;
    use std::time::Instant;

    pub(super) fn measure(
        payload_bytes: usize,
        warmup_blocks: usize,
        measured_blocks: usize,
    ) -> Vec<f64> {
        for _ in 0..warmup_blocks {
            let _ = measure_block(payload_bytes);
        }
        (0..measured_blocks)
            .map(|_| measure_block(payload_bytes))
            .collect()
    }

    fn measure_block(payload_bytes: usize) -> f64 {
        let mut runtime = Runtime::new().expect("create RustJSI external-buffer runtime");
        let owners = payloads(payload_bytes);
        let mut buffers = Vec::with_capacity(BLOCK_SIZE);
        let started = Instant::now();
        workload::publish_block(&mut runtime, owners, &mut buffers);
        let elapsed = started.elapsed();
        workload::clear_properties(&mut runtime);
        workload::shutdown(&mut runtime, &buffers);
        let block_size = u32::try_from(BLOCK_SIZE).expect("profile block size fits in u32");
        elapsed.as_secs_f64() * 1_000_000_000.0 / f64::from(block_size)
    }
}
