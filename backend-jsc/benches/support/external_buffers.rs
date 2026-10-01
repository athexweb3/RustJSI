// SPDX-License-Identifier: MIT OR Apache-2.0

//! Shared owned-external-buffer construction workloads for benchmark probes.

use std::hint::black_box;

pub(crate) const BLOCK_SIZE: usize = 8;

pub(crate) const PROPERTY_NAMES: [&str; BLOCK_SIZE] = [
    "__rustjsi_external_buffer_profile_0",
    "__rustjsi_external_buffer_profile_1",
    "__rustjsi_external_buffer_profile_2",
    "__rustjsi_external_buffer_profile_3",
    "__rustjsi_external_buffer_profile_4",
    "__rustjsi_external_buffer_profile_5",
    "__rustjsi_external_buffer_profile_6",
    "__rustjsi_external_buffer_profile_7",
];

pub(crate) fn payloads(payload_bytes: usize) -> [Box<[u8]>; BLOCK_SIZE] {
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

pub(crate) mod direct {
    use super::{BLOCK_SIZE, PROPERTY_NAMES, black_box};
    use std::ffi::c_void;
    use std::ptr;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

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

    pub(crate) struct ContextOwner {
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
        pub(crate) fn new() -> Self {
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

        pub(crate) fn publish_block(&mut self, owners: [Box<[u8]>; BLOCK_SIZE]) {
            for (name, bytes) in PROPERTY_NAMES.into_iter().zip(owners) {
                black_box(self.externalize(name, bytes));
            }
        }

        pub(crate) fn clear_properties(&self) {
            // SAFETY: This context remains live for the duration of cleanup.
            let undefined = unsafe { JSValueMakeUndefined(self.context) };
            for name in PROPERTY_NAMES {
                let property = StringOwner::new(name);
                let mut exception = ptr::null();
                // SAFETY: Replacing the profile-owned property drops the sole
                // global reachability established by `publish_block`.
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

        pub(crate) fn shutdown(&mut self) {
            // SAFETY: This guard owns the live context. Teardown must release every
            // external allocation still reachable from the profile global object.
            unsafe { JSGlobalContextRelease(self.context) };
            self.context = ptr::null_mut();
            assert_eq!(
                self.reclaimed.load(Ordering::Acquire),
                BLOCK_SIZE,
                "direct external owners must reconcile at context teardown"
            );
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
    }

    impl StringOwner {
        fn new(source: &str) -> Self {
            let source = source.encode_utf16().collect::<Vec<_>>();
            // SAFETY: JavaScriptCore copies the initialized UTF-16 code units.
            let value = unsafe { JSStringCreateWithCharacters(source.as_ptr(), source.len()) };
            assert!(
                !value.is_null(),
                "create direct external-buffer property name"
            );
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

pub(crate) mod rustjsi {
    use super::{BLOCK_SIZE, PROPERTY_NAMES, black_box};
    use rustjsi_backend_jsc::{ExternalBuffer, Runtime};

    pub(crate) fn publish_block(
        runtime: &mut Runtime,
        owners: [Box<[u8]>; BLOCK_SIZE],
        buffers: &mut Vec<ExternalBuffer>,
    ) {
        runtime
            .with_context(|context| {
                for (name, owner) in PROPERTY_NAMES.into_iter().zip(owners) {
                    let buffer = context
                        .install_external_buffer(name, owner)
                        .expect("externalize benchmark payload");
                    black_box(&buffer);
                    buffers.push(buffer);
                }
            })
            .expect("enter RustJSI external-buffer backend");
    }

    pub(crate) fn clear_properties(runtime: &mut Runtime) {
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
    }

    pub(crate) fn shutdown(runtime: &mut Runtime, buffers: &[ExternalBuffer]) {
        runtime
            .invalidate()
            .expect("tear down RustJSI external-buffer runtime");
        assert!(
            buffers.iter().all(ExternalBuffer::is_deallocated),
            "RustJSI external owners must reconcile at runtime teardown"
        );
    }
}
