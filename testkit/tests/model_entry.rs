// SPDX-License-Identifier: MIT OR Apache-2.0

//! Contract checks across borrowed model entries.

use rustjsi_backend::{
    BackendBase, BackendError, BackendScope, BorrowedBufferScope, OwnedExternalBufferScope,
    OwnershipTransferError, RootScope,
};
use rustjsi_testkit::{
    Evaluation, ExternalBufferStats, ModelBackend, Primitive, verify_base_values,
    verify_external_buffer_identity_in_scope,
};
use std::panic::{AssertUnwindSafe, catch_unwind};

#[test]
fn borrowed_entry_keeps_manifest_and_base_semantics() {
    let mut model = ModelBackend::new();
    let manifest = model.manifest();
    model.with_entry(|entry| {
        assert_eq!(entry.manifest(), manifest);
        verify_base_values(entry).unwrap();
    });
    verify_base_values(&mut model).unwrap();
}

#[test]
fn root_identity_survives_adapter_and_direct_scope_boundaries() {
    let mut model = ModelBackend::new();
    let root = model.with_entry(|entry| {
        let scope = entry.open_scope().unwrap();
        let value = scope.string("retained").unwrap();
        scope.persist(value).unwrap()
    });
    {
        let scope = model.open_scope().unwrap();
        assert_eq!(
            scope.to_string(scope.resolve(root).unwrap()).unwrap(),
            "retained"
        );
    }
    model.with_entry(|entry| {
        let scope = entry.open_scope().unwrap();
        assert_eq!(
            scope.to_string(scope.resolve(root).unwrap()).unwrap(),
            "retained"
        );
        scope.release(root).unwrap();
        assert_eq!(scope.resolve(root), Err(BackendError::StaleHandle));
    });
}

#[test]
fn foreign_and_recycled_roots_are_rejected() {
    let mut model = ModelBackend::new();
    let root = {
        let scope = model.open_scope().unwrap();
        scope.persist(scope.number(7.0).unwrap()).unwrap()
    };
    ModelBackend::new().with_entry(|entry| {
        let scope = entry.open_scope().unwrap();
        assert_eq!(scope.resolve(root), Err(BackendError::WrongBackend));
        assert_eq!(scope.release(root), Err(BackendError::WrongBackend));
    });
    model.with_entry(|entry| {
        let scope = entry.open_scope().unwrap();
        scope.release(root).unwrap();
        let replacement = scope.persist(scope.number(8.0).unwrap()).unwrap();
        assert_ne!(root, replacement);
        assert_eq!(scope.resolve(root), Err(BackendError::StaleHandle));
        assert_eq!(scope.release(root), Err(BackendError::StaleHandle));
        assert_eq!(
            scope.as_number(scope.resolve(replacement).unwrap()),
            Ok(8.0)
        );
        scope.release(replacement).unwrap();
    });
}

#[test]
fn buffer_allocation_is_retained_across_entries() {
    let mut model = ModelBackend::new();
    let owner: Box<[u8]> = Box::from([3_u8, 5, 8]);
    let pointer = owner.as_ptr();
    let root = model.with_entry(|entry| {
        let scope = entry.open_scope().unwrap();
        let value = scope.externalize(owner).unwrap();
        {
            let view = scope.buffer_bytes(value).unwrap();
            assert_eq!(view.as_ref(), &[3, 5, 8]);
            assert_eq!(view.as_ref().as_ptr(), pointer);
        }
        scope.persist(value).unwrap()
    });
    assert_eq!(model.external_buffer_stats().live_bytes, 3);
    model.with_entry(|entry| {
        let scope = entry.open_scope().unwrap();
        let value = scope.resolve(root).unwrap();
        {
            let view = scope.buffer_bytes(value).unwrap();
            assert_eq!(view.as_ref().as_ptr(), pointer);
            assert_eq!(view.as_ref(), &[3, 5, 8]);
        }
        scope.release(root).unwrap();
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
fn transfer_failures_preserve_the_ownership_outcome() {
    let mut model = ModelBackend::new();
    let owner: Box<[u8]> = Box::from([13_u8, 21]);
    let pointer = owner.as_ptr();
    model.reject_next_external_buffer();
    let owner = model.with_entry(|entry| {
        let scope = entry.open_scope().unwrap();
        match scope.externalize(owner).unwrap_err() {
            OwnershipTransferError::Rejected { owner, .. } => {
                assert_eq!(owner.as_ptr(), pointer);
                assert_eq!(owner.as_ref(), &[13, 21]);
                owner
            }
            OwnershipTransferError::Accepted { .. } => panic!("unexpected ownership transfer"),
        }
    });
    assert_eq!(
        model.external_buffer_stats(),
        ExternalBufferStats::default()
    );
    model.fail_next_external_buffer_after_accept();
    model.with_entry(|entry| {
        let scope = entry.open_scope().unwrap();
        assert!(matches!(
            scope.externalize(owner),
            Err(OwnershipTransferError::Accepted { .. })
        ));
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
    verify_base_values(&mut model).unwrap();
}

#[test]
fn evaluation_queue_is_shared_with_direct_scopes() {
    let mut model = ModelBackend::new();
    model.push_evaluation(Evaluation::Return(Primitive::Number(34.0)));
    model.push_evaluation(Evaluation::Throw("expected exception".into()));
    model.push_evaluation(Evaluation::Fail("expected failure"));
    model.with_entry(|_| {});
    {
        let scope = model.open_scope().unwrap();
        assert_eq!(
            scope.as_number(scope.evaluate("ignored", "test").unwrap()),
            Ok(34.0)
        );
    }
    model.with_entry(|entry| {
        let scope = entry.open_scope().unwrap();
        match scope.evaluate("ignored", "test").unwrap_err() {
            BackendError::Exception(exception) => {
                assert_eq!(exception.message(), "expected exception");
            }
            error => panic!("unexpected error: {error:?}"),
        }
    });
    let scope = model.open_scope().unwrap();
    assert_eq!(
        scope.evaluate("ignored", "test"),
        Err(BackendError::Failure("expected failure"))
    );
}

#[test]
fn unwind_releases_locals_but_preserves_roots() {
    let mut model = ModelBackend::new();
    let mut root = None;
    assert!(
        catch_unwind(AssertUnwindSafe(|| model.with_entry(|entry| {
            let scope = entry.open_scope().unwrap();
            scope.externalize(Box::from([1_u8, 2])).unwrap();
            let value = scope.externalize(Box::from([3_u8, 4, 5])).unwrap();
            root = Some(scope.persist(value).unwrap());
            panic!("injected unwind");
        })))
        .is_err()
    );
    assert_eq!(
        model.external_buffer_stats(),
        ExternalBufferStats {
            accepted: 2,
            finalized: 1,
            live_bytes: 3,
            copied_bytes: 0,
        }
    );
    model.with_entry(|entry| {
        let scope = entry.open_scope().unwrap();
        scope.release(root.unwrap()).unwrap();
    });
    assert_eq!(
        model.external_buffer_stats(),
        ExternalBufferStats {
            accepted: 2,
            finalized: 2,
            live_bytes: 0,
            copied_bytes: 0,
        }
    );
    model.with_entry(|entry| verify_base_values(entry).unwrap());
}

#[test]
fn overlapping_entries_reject_foreign_values() {
    let mut first = ModelBackend::new();
    let mut second = ModelBackend::new();
    first.with_entry(|entry| {
        let scope = entry.open_scope().unwrap();
        let value = scope.number(55.0).unwrap();
        second.with_entry(|other| {
            let scope = other.open_scope().unwrap();
            assert_eq!(scope.as_number(value), Err(BackendError::WrongBackend));
            assert_eq!(scope.persist(value), Err(BackendError::WrongBackend));
        });
        assert_eq!(scope.as_number(value), Ok(55.0));
    });
}

#[test]
fn scoped_buffer_conformance_accepts_borrowed_entries() {
    let mut model = ModelBackend::new();
    model.with_entry(|entry| {
        let scope = entry.open_scope().unwrap();
        verify_external_buffer_identity_in_scope(&scope).unwrap();
        verify_external_buffer_identity_in_scope(&scope).unwrap();
    });
    assert_eq!(
        model.external_buffer_stats(),
        ExternalBufferStats {
            accepted: 2,
            finalized: 2,
            live_bytes: 0,
            copied_bytes: 0,
        }
    );
}

#[test]
fn scoped_buffer_conformance_returns_transfer_failures() {
    let mut model = ModelBackend::new();
    model.reject_next_external_buffer();
    model.with_entry(|entry| {
        let scope = entry.open_scope().unwrap();
        assert_eq!(
            verify_external_buffer_identity_in_scope(&scope),
            Err(BackendError::Failure("injected external-buffer rejection"))
        );
    });
    assert_eq!(
        model.external_buffer_stats(),
        ExternalBufferStats::default()
    );
    model.fail_next_external_buffer_after_accept();
    model.with_entry(|entry| {
        let scope = entry.open_scope().unwrap();
        assert_eq!(
            verify_external_buffer_identity_in_scope(&scope),
            Err(BackendError::Failure(
                "injected failure after ownership transfer"
            ))
        );
        verify_external_buffer_identity_in_scope(&scope).unwrap();
    });
    assert_eq!(
        model.external_buffer_stats(),
        ExternalBufferStats {
            accepted: 2,
            finalized: 2,
            live_bytes: 0,
            copied_bytes: 0,
        }
    );
}
