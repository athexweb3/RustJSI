// SPDX-License-Identifier: MIT OR Apache-2.0

//! Scope families over legal JSC host entries.

#![cfg(all(feature = "experimental-jsc", target_os = "macos"))]

use rustjsi_backend::{
    BackendBase, BackendError, BackendFamily, BackendScope, OwnedExternalBufferScope,
    OwnershipTransferError, RootBackend, RootScope, ValueKind,
};
use rustjsi_backend_jsc::{
    CallLimits, ExternalBufferLimits, HostFunctionLimits, InboundCallbackLimits, JscBackendFamily,
    JscRuntimeLimits, NativeStateInstallError, NativeStateLimits, RootLimits, Runtime,
    RuntimeError, Value,
};
use rustjsi_host::{Host, HostState, ResourceLedger};
use rustjsi_testkit::{
    ModelBackend, ModelBackendFamily, create_number_root, verify_base_values,
    verify_number_root_and_release,
};
use std::cell::Cell;
use std::panic::{AssertUnwindSafe, catch_unwind};

fn create<F, R>(backend: &mut F::Backend<'_>) -> Result<R, BackendError>
where
    F: BackendFamily,
    for<'scope> F::Backend<'scope>: RootBackend<Root = R>,
    for<'scope> F::Scope<'scope>: RootScope + OwnedExternalBufferScope,
{
    F::try_with_scope(backend, |scope| {
        let buffer = scope
            .externalize(Box::from([1_u8, 2, 3]))
            .map_err(|error| error.error().clone())?;
        assert_eq!(scope.kind(buffer)?, ValueKind::Buffer);
        create_number_root(&scope)
    })
}

fn check<F, R>(backend: &mut F::Backend<'_>, root: R) -> Result<(), BackendError>
where
    F: BackendFamily,
    for<'scope> F::Backend<'scope>: RootBackend<Root = R>,
    for<'scope> F::Scope<'scope>: RootScope,
{
    F::try_with_scope(backend, |scope| {
        verify_number_root_and_release(&scope, root)
    })
}

fn verify_source_host<H>(host: &mut H) -> Result<(), RuntimeError>
where
    H: Host<Family = JscBackendFamily, Error = RuntimeError>,
{
    let attachment = host.attachment_id();
    assert_eq!(host.state(), HostState::Active);
    host.with_backend(|backend| verify_base_values(backend))?
        .expect("JSC must satisfy base conformance");
    assert_eq!(host.attachment_id(), attachment);
    Ok(())
}

#[test]
fn both_families_use_the_same_capability_consumers() {
    let mut model = ModelBackend::new();
    let root = model.with_entry(create::<ModelBackendFamily, _>).unwrap();
    model
        .with_entry(|backend| check::<ModelBackendFamily, _>(backend, root))
        .unwrap();
    assert_eq!(model.external_buffer_stats().finalized, 1);

    let mut runtime = Runtime::new().unwrap();
    let root = runtime
        .with_backend(create::<JscBackendFamily, _>)
        .unwrap()
        .unwrap();
    runtime
        .with_backend(|backend| check::<JscBackendFamily, _>(backend, root))
        .unwrap()
        .unwrap();
}

#[test]
fn public_call_limit_configuration_is_available_to_owned_hosts() {
    let mut runtime = Runtime::new_with_limits(
        RootLimits::default(),
        CallLimits {
            arguments: 0,
            string_utf8_bytes: 0,
        },
    )
    .unwrap();
    runtime.invalidate().unwrap();
}

#[test]
fn public_inbound_callback_configuration_rejects_before_string_escapes() {
    let mut runtime = Runtime::new_with_jsc_limits(JscRuntimeLimits {
        inbound_callback: InboundCallbackLimits {
            arguments: 1,
            string_utf8_bytes: 4,
        },
        ..JscRuntimeLimits::default()
    })
    .unwrap();
    let observed = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));

    runtime
        .with_context(|cx| {
            let callback_observed = std::rc::Rc::clone(&observed);
            cx.install_host_function("publicInbound", move |call| {
                let value = call.string(0)?;
                callback_observed.borrow_mut().push(value.clone());
                Ok(Value::String(value))
            })
            .unwrap();

            let error = cx
                .eval("publicInbound('🦀x')", "public-inbound-limit.js")
                .unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("inbound callback string data limit reached")
            );
            assert!(observed.borrow().is_empty());
        })
        .unwrap();
}

#[test]
fn public_native_state_recovery_api_installs_and_leases_state() {
    let mut runtime = Runtime::new().unwrap();
    runtime
        .with_context(|cx| {
            let state = match cx.try_install_native_state("publicState", String::from("state")) {
                Ok(state) => state,
                Err(NativeStateInstallError::Rejected { error, .. }) => {
                    panic!("active runtime unexpectedly rejected state: {error}")
                }
                Err(NativeStateInstallError::Js(error)) => {
                    panic!("state publication unexpectedly failed: {error}")
                }
            };
            assert_eq!(cx.with_native_state(&state, Clone::clone).unwrap(), "state");
        })
        .unwrap();
}

#[test]
fn public_native_state_limit_returns_the_unaccepted_owner() {
    let mut runtime = Runtime::new_with_jsc_limits(JscRuntimeLimits {
        native_states: NativeStateLimits { registrations: 0 },
        ..JscRuntimeLimits::default()
    })
    .unwrap();

    runtime
        .with_context(|cx| {
            let owner = String::from("state");
            let address = owner.as_ptr();
            match cx.try_install_native_state("publicLimitedState", owner) {
                Err(NativeStateInstallError::Rejected { state, error }) => {
                    assert_eq!(error, RuntimeError::NativeStateRegistrationLimitReached);
                    assert_eq!(state.as_ptr(), address);
                    assert_eq!(state, "state");
                }
                result => panic!("expected native-state admission refusal: {result:?}"),
            }
        })
        .unwrap();
}

#[test]
fn public_host_function_limit_rejects_before_callback_capture() {
    let mut runtime = Runtime::new_with_jsc_limits(JscRuntimeLimits {
        host_functions: HostFunctionLimits { registrations: 0 },
        ..JscRuntimeLimits::default()
    })
    .unwrap();
    let capture = std::rc::Rc::new(());

    runtime
        .with_context(|cx| {
            let captured = std::rc::Rc::clone(&capture);
            assert!(matches!(
                cx.install_host_function("publicLimitedCallback", move |_| {
                    std::hint::black_box(&captured);
                    Ok(Value::Undefined)
                }),
                Err(rustjsi_backend_jsc::JsError::Runtime(
                    RuntimeError::HostFunctionLimitReached
                ))
            ));
            assert_eq!(std::rc::Rc::strong_count(&capture), 1);
        })
        .unwrap();
}

#[test]
fn public_resource_snapshot_tracks_long_lived_resources_and_pending_release() {
    let mut runtime = Runtime::new().unwrap();
    let initial = runtime.resource_snapshot().unwrap();
    assert_eq!(initial.persistent_roots(), 0);
    assert_eq!(initial.pending_persistent_releases(), 0);
    assert_eq!(initial.host_function_registrations(), 0);
    assert_eq!(initial.native_state_registrations(), 0);
    assert_eq!(initial.pending_native_finalizers(), 0);
    assert_eq!(initial.external_buffer_allocations(), 0);
    assert_eq!(initial.external_buffer_bytes(), 0);
    assert_eq!(initial.callback_drop_panics(), 0);
    assert_eq!(initial.native_state_drop_panics(), 0);
    assert_eq!(initial.resources(), ResourceLedger::default());

    let root = runtime
        .with_context(|cx| {
            let local = cx
                .eval("({ marker: 'snapshot' })", "snapshot-root.js")
                .unwrap();
            let root = cx.persist(&local).unwrap();
            cx.install_host_function("snapshotCallback", |_| Ok(Value::Undefined))
                .unwrap();
            let _state = cx.install_native_state("snapshotState", 7_u8).unwrap();
            let _buffer = cx
                .install_external_buffer("snapshotBytes", vec![1_u8, 2, 3].into_boxed_slice())
                .unwrap();
            root
        })
        .unwrap();

    let retained = runtime.resource_snapshot().unwrap();
    assert_eq!(retained.persistent_roots(), 1);
    assert_eq!(retained.pending_persistent_releases(), 0);
    assert_eq!(retained.host_function_registrations(), 1);
    assert_eq!(retained.native_state_registrations(), 1);
    assert_eq!(retained.pending_native_finalizers(), 0);
    assert_eq!(retained.external_buffer_allocations(), 1);
    assert_eq!(retained.external_buffer_bytes(), 3);
    assert_eq!(retained.resources(), ResourceLedger::new(1, 1, 1, 1, 3));

    drop(root);
    let pending = runtime.resource_snapshot().unwrap();
    assert_eq!(pending.persistent_roots(), 1);
    assert_eq!(pending.pending_persistent_releases(), 1);
    assert_eq!(pending.pending_native_finalizers(), 0);
    assert_eq!(pending.resources(), ResourceLedger::new(1, 1, 1, 1, 3));

    runtime.with_context(|_| {}).unwrap();
    let released = runtime.resource_snapshot().unwrap();
    assert_eq!(released.persistent_roots(), 0);
    assert_eq!(released.pending_persistent_releases(), 0);
    assert_eq!(released.host_function_registrations(), 1);
    assert_eq!(released.native_state_registrations(), 1);
    assert_eq!(released.pending_native_finalizers(), 0);
    assert_eq!(released.external_buffer_allocations(), 1);
    assert_eq!(released.external_buffer_bytes(), 3);
    assert_eq!(released.resources(), ResourceLedger::new(0, 1, 1, 1, 3));
}

#[test]
fn public_external_buffer_limits_reject_before_ownership_transfer() {
    let mut runtime = Runtime::new_with_external_buffer_limits(
        RootLimits::default(),
        CallLimits::default(),
        ExternalBufferLimits {
            allocations: 0,
            bytes: usize::MAX,
        },
    )
    .unwrap();
    runtime
        .with_backend(|backend| {
            let scope = backend.open_scope().unwrap();
            let owner = vec![9_u8, 8, 7].into_boxed_slice();
            let pointer = owner.as_ptr();
            match scope.externalize(owner) {
                Err(OwnershipTransferError::Rejected { owner, error }) => {
                    assert_eq!(owner.as_ptr(), pointer);
                    assert_eq!(&*owner, &[9, 8, 7]);
                    assert_eq!(
                        error,
                        BackendError::Failure("external-buffer allocation limit reached")
                    );
                }
                other => panic!("unexpected transfer result: {other:?}"),
            }
            scope.string("scope remains usable").unwrap();
        })
        .unwrap();
}

#[test]
fn public_external_buffer_byte_limit_rejects_before_ownership_transfer() {
    let mut runtime = Runtime::new_with_external_buffer_limits(
        RootLimits::default(),
        CallLimits::default(),
        ExternalBufferLimits {
            allocations: 1,
            bytes: 0,
        },
    )
    .unwrap();
    runtime
        .with_backend(|backend| {
            let scope = backend.open_scope().unwrap();
            let owner = vec![9_u8, 8, 7].into_boxed_slice();
            let pointer = owner.as_ptr();
            match scope.externalize(owner) {
                Err(OwnershipTransferError::Rejected { owner, error }) => {
                    assert_eq!(owner.as_ptr(), pointer);
                    assert_eq!(&*owner, &[9, 8, 7]);
                    assert_eq!(
                        error,
                        BackendError::Failure("external-buffer byte limit reached")
                    );
                }
                other => panic!("unexpected transfer result: {other:?}"),
            }
            scope.string("scope remains usable").unwrap();
        })
        .unwrap();
}

#[test]
fn owning_and_borrowed_jsc_hosts_share_the_source_contract() {
    let mut runtime = Runtime::new().unwrap();
    verify_source_host(&mut runtime).unwrap();
    verify_source_host(&mut &mut runtime).unwrap();
}

#[test]
fn fallible_scope_preserves_javascript_exceptions() {
    let mut runtime = Runtime::new().unwrap();
    let result = runtime
        .with_backend(|backend| {
            JscBackendFamily::try_with_scope(backend, |scope| {
                scope.evaluate("throw new Error('family failure')", "family.js")?;
                Ok(())
            })
        })
        .unwrap();
    match result {
        Err(BackendError::Exception(exception)) => {
            assert!(exception.message().contains("family failure"));
        }
        result => panic!("unexpected result: {result:?}"),
    }
    let root = runtime
        .with_backend(create::<JscBackendFamily, _>)
        .unwrap()
        .unwrap();
    runtime
        .with_backend(|backend| check::<JscBackendFamily, _>(backend, root))
        .unwrap()
        .unwrap();
}

#[test]
fn roots_remain_instance_bound_and_host_invalidation_blocks_entry() {
    let mut runtime = Runtime::new().unwrap();
    let root = runtime
        .with_backend(create::<JscBackendFamily, _>)
        .unwrap()
        .unwrap();
    let mut other = Runtime::new().unwrap();
    assert_eq!(
        other
            .with_backend(|backend| check::<JscBackendFamily, _>(backend, root))
            .unwrap(),
        Err(BackendError::WrongBackend)
    );
    runtime
        .with_backend(|backend| check::<JscBackendFamily, _>(backend, root))
        .unwrap()
        .unwrap();
    assert_eq!(
        runtime
            .with_backend(|backend| check::<JscBackendFamily, _>(backend, root))
            .unwrap(),
        Err(BackendError::StaleHandle)
    );

    runtime.invalidate().unwrap();
    let called = Cell::new(false);
    assert_eq!(
        runtime.with_backend(|backend| {
            JscBackendFamily::with_scope(backend, |_| called.set(true))
        }),
        Err(RuntimeError::Invalidated)
    );
    assert!(!called.get());
}

#[test]
fn panic_in_family_scope_allows_later_host_entry() {
    let mut runtime = Runtime::new().unwrap();
    assert!(
        catch_unwind(AssertUnwindSafe(|| {
            runtime
                .with_backend(|backend| {
                    JscBackendFamily::with_scope(backend, |scope| {
                        scope.string("rooted local").unwrap();
                        panic!("injected unwind");
                    })
                })
                .unwrap()
                .unwrap();
        }))
        .is_err()
    );
    let root = runtime
        .with_backend(create::<JscBackendFamily, _>)
        .unwrap()
        .unwrap();
    runtime
        .with_backend(|backend| check::<JscBackendFamily, _>(backend, root))
        .unwrap()
        .unwrap();
}
