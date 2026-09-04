// SPDX-License-Identifier: MIT OR Apache-2.0

//! Scope-family capability composition on the deterministic backend.

use rustjsi_backend::{
    BackendError, BackendFamily, BorrowedBufferScope, OwnedExternalBufferScope, RootBackend,
    RootScope,
};
use rustjsi_testkit::{ExternalBufferStats, ModelBackend, ModelBackendFamily};
use std::panic::{AssertUnwindSafe, catch_unwind};

fn retain<F, R>(backend: &mut F::Backend<'_>, owner: Box<[u8]>) -> Result<R, BackendError>
where
    F: BackendFamily,
    for<'scope> F::Backend<'scope>: RootBackend<Root = R>,
    for<'scope> F::Scope<'scope>: RootScope + OwnedExternalBufferScope,
{
    F::with_scope(backend, |scope| {
        let value = scope
            .externalize(owner)
            .map_err(|error| error.error().clone())?;
        scope.persist(value)
    })?
}

fn check_and_release<F, R>(
    backend: &mut F::Backend<'_>,
    root: R,
    pointer: *const u8,
) -> Result<(), BackendError>
where
    F: BackendFamily,
    R: Copy,
    for<'scope> F::Backend<'scope>: RootBackend<Root = R>,
    for<'scope> F::Scope<'scope>: RootScope + BorrowedBufferScope,
{
    F::with_scope(backend, |scope| {
        let value = scope.resolve(root)?;
        {
            let view = scope.buffer_bytes(value)?;
            assert_eq!(view.as_ref(), &[2, 3, 5]);
            assert_eq!(view.as_ref().as_ptr(), pointer);
        }
        scope.release(root)
    })?
}

fn retain_with_view<F, R>(backend: &mut F::Backend<'_>) -> Result<R, BackendError>
where
    F: BackendFamily,
    for<'scope> F::Backend<'scope>: RootBackend<Root = R>,
    for<'scope> F::Scope<'scope>: RootScope + OwnedExternalBufferScope + BorrowedBufferScope,
{
    F::with_scope(backend, |scope| {
        let owner: Box<[u8]> = Box::from([2, 3, 5]);
        let pointer = owner.as_ptr();
        let value = scope
            .externalize(owner)
            .map_err(|error| error.error().clone())?;
        {
            let view = scope.buffer_bytes(value)?;
            assert_eq!(view.as_ref().as_ptr(), pointer);
            assert_eq!(view.as_ref(), &[2, 3, 5]);
        }
        scope.persist(value)
    })?
}

#[test]
fn three_capabilities_are_available_in_one_generic_scope() {
    let mut model = ModelBackend::new();
    let root = model
        .with_entry(retain_with_view::<ModelBackendFamily, _>)
        .unwrap();
    assert_eq!(model.external_buffer_stats().live_bytes, 3);
    model
        .with_entry(|backend| {
            ModelBackendFamily::with_scope(backend, |scope| {
                scope.release(root).unwrap();
            })
        })
        .unwrap();
    assert_eq!(model.external_buffer_stats().live_bytes, 0);
    assert_eq!(model.external_buffer_stats().finalized, 1);
}

#[test]
fn capabilities_compose_across_entries_without_changing_the_allocation() {
    let mut model = ModelBackend::new();
    let owner: Box<[u8]> = Box::from([2, 3, 5]);
    let pointer = owner.as_ptr();
    let root = model
        .with_entry(|backend| retain::<ModelBackendFamily, _>(backend, owner))
        .unwrap();
    ModelBackend::new().with_entry(|backend| {
        assert_eq!(
            check_and_release::<ModelBackendFamily, _>(backend, root, pointer),
            Err(BackendError::WrongBackend)
        );
    });
    model
        .with_entry(|backend| check_and_release::<ModelBackendFamily, _>(backend, root, pointer))
        .unwrap();
    model.with_entry(|backend| {
        assert_eq!(
            check_and_release::<ModelBackendFamily, _>(backend, root, pointer),
            Err(BackendError::StaleHandle)
        );
    });
    assert_eq!(
        model.external_buffer_stats(),
        ExternalBufferStats {
            accepted: 1,
            finalized: 1,
            live_bytes: 0,
            copied_bytes: 0,
        }
    );
}

#[test]
fn family_scope_cleanup_runs_during_unwind() {
    let mut model = ModelBackend::new();
    assert!(
        catch_unwind(AssertUnwindSafe(|| model.with_entry(|backend| {
            ModelBackendFamily::with_scope(backend, |scope| {
                scope.externalize(Box::from([1_u8, 2, 3])).unwrap();
                panic!("injected unwind");
            })
            .unwrap();
        })))
        .is_err()
    );
    assert_eq!(
        model.external_buffer_stats(),
        ExternalBufferStats {
            accepted: 1,
            finalized: 1,
            live_bytes: 0,
            copied_bytes: 0,
        }
    );
    model
        .with_entry(|backend| {
            ModelBackendFamily::with_scope(backend, |scope| {
                rustjsi_testkit::verify_external_buffer_identity_in_scope(&scope).unwrap();
            })
        })
        .unwrap();
    assert_eq!(model.external_buffer_stats().finalized, 2);
}
