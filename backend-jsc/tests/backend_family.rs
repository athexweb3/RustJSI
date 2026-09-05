// SPDX-License-Identifier: MIT OR Apache-2.0

//! Scope families over legal JSC host entries.

#![cfg(all(feature = "experimental-jsc", target_os = "macos"))]

use rustjsi_backend::{
    BackendError, BackendFamily, BackendScope, OwnedExternalBufferScope, RootBackend, RootScope,
    ValueKind,
};
use rustjsi_backend_jsc::{JscBackendFamily, Runtime, RuntimeError};
use rustjsi_host::{Host, HostState};
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
