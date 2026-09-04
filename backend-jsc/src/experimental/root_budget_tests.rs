// SPDX-License-Identifier: MIT OR Apache-2.0

use super::{JsError, Runtime, RuntimeError};
use rustjsi_backend::{BackendBase, BackendError, BackendScope, RootScope};
use std::rc::Rc;

#[test]
fn legacy_and_common_roots_compete_for_the_same_slots() {
    let mut runtime = Runtime::new_with_persistent_root_limit(2).unwrap();
    let legacy = runtime
        .with_context(|cx| {
            let local = cx.eval("'legacy'", "budget.js").unwrap();
            cx.persist(&local).unwrap()
        })
        .unwrap();
    let common = runtime
        .with_backend(|backend| {
            let scope = backend.open_scope().unwrap();
            let value = scope.number(42.0).unwrap();
            let root = scope.persist(value).unwrap();
            assert_eq!(
                scope.persist(value),
                Err(BackendError::Failure("persistent root slot limit reached"))
            );
            root
        })
        .unwrap();
    runtime
        .with_context(|cx| {
            let local = cx.resolve(&legacy).unwrap();
            assert_eq!(cx.string(&local).unwrap(), "legacy");
            assert_eq!(
                cx.persist(&local).unwrap_err(),
                JsError::Runtime(RuntimeError::PersistentRootLimitReached)
            );
        })
        .unwrap();
    drop(legacy);
    runtime
        .with_backend(|backend| {
            let scope = backend.open_scope().unwrap();
            let value = scope.resolve(common).unwrap();
            assert_eq!(scope.as_number(value).unwrap(), 42.0);
            let replacement = scope.persist(value).unwrap();
            scope.release(common).unwrap();
            scope.release(replacement).unwrap();
        })
        .unwrap();
}

#[test]
fn lease_clones_share_capacity_but_last_drop_does_not_release_it_early() {
    let mut runtime = Runtime::new_with_persistent_root_limit(1).unwrap();
    runtime
        .with_context(|cx| {
            let local = cx.eval("({})", "cloned-root.js").unwrap();
            let root = cx.persist(&local).unwrap();
            let clone = root.clone();
            drop(root);
            assert!(cx.resolve(&clone).is_ok());
            assert!(cx.persist(&local).is_err());
            drop(clone);
            assert!(cx.persist(&local).is_err());
            assert_eq!(cx.shared.roots.borrow().slots.len(), 1);
        })
        .unwrap();
    runtime
        .with_context(|cx| {
            let local = cx.eval("({})", "reuse.js").unwrap();
            assert!(cx.persist(&local).is_ok());
        })
        .unwrap();
}

#[test]
fn full_pending_budget_survives_unwind_and_is_recovered_on_next_entry() {
    let mut runtime = Runtime::new_with_persistent_root_limit(2).unwrap();
    let shared = Rc::clone(&runtime.shared);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        runtime
            .with_context(|cx| {
                let local = cx.eval("({})", "unwind.js").unwrap();
                let _first = cx.persist(&local).unwrap();
                let _second = cx.persist(&local).unwrap();
                panic!("full root budget");
            })
            .unwrap();
    }));
    assert!(result.is_err());
    assert!(shared.roots.borrow().pending_head.is_some());
    runtime
        .with_backend(|backend| {
            let scope = backend.open_scope().unwrap();
            assert!(shared.roots.borrow().pending_head.is_none());
            let value = scope.number(1.0).unwrap();
            scope.persist(value).unwrap();
            scope.persist(value).unwrap();
            assert!(scope.persist(value).is_err());
        })
        .unwrap();
    runtime.invalidate().unwrap();
    assert!(
        shared
            .roots
            .borrow()
            .slots
            .iter()
            .all(|slot| slot.value.is_none())
    );
    assert!(shared.roots.borrow().pending_head.is_none());
}
