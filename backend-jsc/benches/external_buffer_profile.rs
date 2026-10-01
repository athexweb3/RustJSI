// SPDX-License-Identifier: MIT OR Apache-2.0

//! Matched direct and `RustJSI` owned-external-buffer construction profile.

#[cfg(target_os = "macos")]
fn main() {
    const BLOCK_SIZE: usize = 8;
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
            Workload::Direct => {
                direct::measure(payload_bytes, BLOCK_SIZE, WARMUP_BLOCKS, MEASURED_BLOCKS)
            }
            Workload::RustJsi => {
                rustjsi::measure(payload_bytes, BLOCK_SIZE, WARMUP_BLOCKS, MEASURED_BLOCKS)
            }
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
    print_samples("direct_jsc_external_buffer", BLOCK_SIZE, &direct);
    print_samples("rustjsi_external_buffer", BLOCK_SIZE, &rustjsi);
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
fn print_samples(name: &str, block_size: usize, samples: &[f64]) {
    let samples = samples
        .iter()
        .map(|sample| format!("{sample:.4}"))
        .collect::<Vec<_>>()
        .join(",");
    println!("{name}: {block_size} ops/batch {samples} ns/transfer");
}

#[cfg(target_os = "macos")]
fn mean(samples: &[f64]) -> f64 {
    let sample_count = u32::try_from(samples.len()).expect("sample count fits in u32");
    samples.iter().sum::<f64>() / f64::from(sample_count)
}

#[cfg(target_os = "macos")]
fn payloads<const BLOCK_SIZE: usize>(payload_bytes: usize) -> [Box<[u8]>; BLOCK_SIZE] {
    std::array::from_fn(|index| {
        let mut bytes = vec![0; payload_bytes].into_boxed_slice();
        if let Some(first) = bytes.first_mut() {
            *first = u8::try_from(index)
                .expect("profile block index fits in u8")
                .wrapping_add(0x3c);
        }
        bytes
    })
}

#[cfg(target_os = "macos")]
const PROPERTY_NAMES: [&str; 8] = [
    "__rustjsi_external_buffer_profile_0",
    "__rustjsi_external_buffer_profile_1",
    "__rustjsi_external_buffer_profile_2",
    "__rustjsi_external_buffer_profile_3",
    "__rustjsi_external_buffer_profile_4",
    "__rustjsi_external_buffer_profile_5",
    "__rustjsi_external_buffer_profile_6",
    "__rustjsi_external_buffer_profile_7",
];

#[cfg(target_os = "macos")]
mod direct {
    use super::{PROPERTY_NAMES, payloads};
    use std::ffi::c_void;
    use std::hint::black_box;
    use std::ptr;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Instant;

    type Context = *mut c_void;
    type Value = *const c_void;

    #[link(name = "JavaScriptCore", kind = "framework")]
    unsafe extern "C" {
        fn JSGlobalContextCreate(class: *mut c_void) -> Context;
        fn JSGlobalContextRelease(context: Context);
        fn JSStringCreateWithCharacters(chars: *const u16, length: usize) -> *mut c_void;
        fn JSStringRelease(string: *mut c_void);
        fn JSContextGetGlobalObject(context: Context) -> *mut c_void;
        fn JSValueMakeUndefined(context: Context) -> Value;
        fn JSObjectSetProperty(
            context: Context,
            object: *mut c_void,
            property_name: *mut c_void,
            value: Value,
            attributes: u32,
            exception: *mut Value,
        );
        fn JSObjectMakeArrayBufferWithBytesNoCopy(
            context: Context,
            bytes: *mut c_void,
            byte_length: usize,
            deallocator: Option<unsafe extern "C" fn(*mut c_void, *mut c_void)>,
            deallocator_context: *mut c_void,
            exception: *mut Value,
        ) -> *mut c_void;
    }

    pub(super) fn measure(
        payload_bytes: usize,
        block_size: usize,
        warmup_blocks: usize,
        measured_blocks: usize,
    ) -> Vec<f64> {
        for _ in 0..warmup_blocks {
            let _ = measure_block(payload_bytes, block_size);
        }
        (0..measured_blocks)
            .map(|_| measure_block(payload_bytes, block_size))
            .collect()
    }

    fn measure_block(payload_bytes: usize, block_size: usize) -> f64 {
        let mut context = ContextOwner::new();
        let sample = context.transfer_block(payload_bytes, block_size);
        context.shutdown();
        sample
    }

    struct ContextOwner {
        context: Context,
        global: *mut c_void,
        reclaimed: Arc<AtomicUsize>,
    }

    struct StringOwner(*mut c_void);

    struct ExternalOwner {
        _bytes: Box<[u8]>,
        reclaimed: Arc<AtomicUsize>,
    }

    impl ContextOwner {
        fn new() -> Self {
            // SAFETY: A null class selects JavaScriptCore's default global class.
            let context = unsafe { JSGlobalContextCreate(ptr::null_mut()) };
            assert!(!context.is_null(), "create direct JSC context");
            // SAFETY: A live global context always owns a global object.
            let global = unsafe { JSContextGetGlobalObject(context) };
            assert!(!global.is_null(), "get direct JSC global object");
            Self {
                context,
                global,
                reclaimed: Arc::new(AtomicUsize::new(0)),
            }
        }

        fn transfer_block(&mut self, payload_bytes: usize, block_size: usize) -> f64 {
            assert_eq!(
                block_size, 8,
                "profile block size is fixed for this executable"
            );
            let owners = payloads::<8>(payload_bytes);
            let started = Instant::now();
            for (name, bytes) in PROPERTY_NAMES.into_iter().zip(owners) {
                black_box(self.externalize(name, bytes));
            }
            let elapsed = started.elapsed();

            self.clear_properties();
            elapsed.as_secs_f64() * 1_000_000_000.0 / 8.0
        }

        fn externalize(&self, name: &str, bytes: Box<[u8]>) -> Value {
            let byte_length = bytes.len();
            let mut bytes = bytes;
            let origin = bytes.as_mut_ptr();
            let owner = Box::new(ExternalOwner {
                _bytes: bytes,
                reclaimed: Arc::clone(&self.reclaimed),
            });
            let owner = Box::into_raw(owner);
            let mut exception = ptr::null();
            // SAFETY: The owner uniquely owns the exact buffer described by
            // `origin` and transfers it to JavaScriptCore for either successful
            // construction or an exception path.
            let object = unsafe {
                JSObjectMakeArrayBufferWithBytesNoCopy(
                    self.context,
                    origin.cast(),
                    byte_length,
                    Some(external_bytes_deallocator),
                    owner.cast(),
                    &raw mut exception,
                )
            };
            assert!(
                exception.is_null(),
                "direct external buffer construction threw"
            );
            assert!(
                !object.is_null(),
                "direct external buffer construction returned null"
            );
            let property = StringOwner::new(name);
            let mut exception = ptr::null();
            // SAFETY: The global object, property name, and constructed object all
            // belong to this live context. Publication retains the ArrayBuffer.
            unsafe {
                JSObjectSetProperty(
                    self.context,
                    self.global,
                    property.0,
                    object.cast_const(),
                    0,
                    &raw mut exception,
                );
            }
            assert!(exception.is_null(), "publish direct external buffer");
            object.cast_const()
        }

        fn clear_properties(&self) {
            // SAFETY: This context remains live for the duration of cleanup.
            let undefined = unsafe { JSValueMakeUndefined(self.context) };
            for name in PROPERTY_NAMES {
                let property = StringOwner::new(name);
                let mut exception = ptr::null();
                // SAFETY: Replacing the profile-owned property drops the sole
                // global reachability established by `externalize`.
                unsafe {
                    JSObjectSetProperty(
                        self.context,
                        self.global,
                        property.0,
                        undefined,
                        0,
                        &raw mut exception,
                    );
                }
                assert!(exception.is_null(), "clear direct external buffer property");
            }
        }

        fn shutdown(&mut self) {
            // SAFETY: This guard owns the live context. Teardown must release every
            // external allocation still reachable from the profile global object.
            unsafe { JSGlobalContextRelease(self.context) };
            self.context = ptr::null_mut();
            assert_eq!(
                self.reclaimed.load(Ordering::Acquire),
                8,
                "direct external owners must reconcile at context teardown"
            );
        }
    }

    impl StringOwner {
        fn new(source: &str) -> Self {
            let source = source.encode_utf16().collect::<Vec<_>>();
            // SAFETY: JavaScriptCore copies the initialized UTF-16 code units.
            let value = unsafe { JSStringCreateWithCharacters(source.as_ptr(), source.len()) };
            assert!(!value.is_null(), "create direct collection churn source");
            Self(value)
        }
    }

    impl Drop for StringOwner {
        fn drop(&mut self) {
            // SAFETY: This guard owns one successful JavaScriptCore string creation.
            unsafe { JSStringRelease(self.0) };
        }
    }

    impl Drop for ContextOwner {
        fn drop(&mut self) {
            if self.context.is_null() {
                return;
            }
            // SAFETY: This guard owns the context and its remaining external
            // allocations. Context destruction invokes their registered deleters.
            unsafe { JSGlobalContextRelease(self.context) };
        }
    }

    unsafe extern "C" fn external_bytes_deallocator(_bytes: *mut c_void, owner: *mut c_void) {
        if owner.is_null() {
            return;
        }
        // SAFETY: JavaScriptCore invokes this once for the unique owner transferred
        // at successful construction or construction failure.
        let owner = unsafe { Box::from_raw(owner.cast::<ExternalOwner>()) };
        owner.reclaimed.fetch_add(1, Ordering::AcqRel);
        drop(owner);
    }
}

#[cfg(target_os = "macos")]
mod rustjsi {
    use super::{PROPERTY_NAMES, payloads};
    use rustjsi_backend_jsc::{ExternalBuffer, Runtime};
    use std::hint::black_box;
    use std::time::Instant;

    pub(super) fn measure(
        payload_bytes: usize,
        block_size: usize,
        warmup_blocks: usize,
        measured_blocks: usize,
    ) -> Vec<f64> {
        for _ in 0..warmup_blocks {
            let _ = transfer_block(payload_bytes, block_size);
        }
        (0..measured_blocks)
            .map(|_| transfer_block(payload_bytes, block_size))
            .collect()
    }

    fn transfer_block(payload_bytes: usize, block_size: usize) -> f64 {
        assert_eq!(
            block_size, 8,
            "profile block size is fixed for this executable"
        );
        let mut runtime = Runtime::new().expect("create RustJSI external-buffer runtime");
        let owners = payloads::<8>(payload_bytes);
        let mut buffers = Vec::with_capacity(8);
        let elapsed = runtime
            .with_context(|context| {
                let started = Instant::now();
                for (name, owner) in PROPERTY_NAMES.into_iter().zip(owners) {
                    let buffer = context
                        .install_external_buffer(name, owner)
                        .expect("externalize benchmark payload");
                    black_box(&buffer);
                    buffers.push(buffer);
                }
                started.elapsed()
            })
            .expect("enter RustJSI external-buffer backend");
        runtime
            .with_context(|context| {
                for name in PROPERTY_NAMES {
                    context
                        .eval(
                            &format!("delete globalThis.{name}"),
                            "external-buffer-profile-delete.js",
                        )
                        .expect("remove external-buffer profile reachability");
                }
            })
            .expect("clear RustJSI external-buffer profile properties");
        runtime
            .invalidate()
            .expect("tear down RustJSI external-buffer runtime");
        assert!(
            buffers.iter().all(ExternalBuffer::is_deallocated),
            "RustJSI external owners must reconcile at runtime teardown"
        );
        elapsed.as_secs_f64() * 1_000_000_000.0 / 8.0
    }
}
