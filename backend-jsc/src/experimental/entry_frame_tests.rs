// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;

#[test]
fn active_entry_frame_restores_outer_state_after_nested_entry() {
    let runtime = Runtime::new().unwrap();
    let shared = Rc::clone(&runtime.shared);
    let context = runtime.context.unwrap();

    let outer = ActiveEntryFrame::enter(&runtime.shared, context).unwrap();
    assert_eq!(shared.gate.active_entries(), 1);
    assert_eq!(ACTIVE_RUNTIME.with(Cell::get), Rc::as_ptr(&shared));
    assert_eq!(ACTIVE_CONTEXT.with(Cell::get), context.as_ptr());

    let inner = ActiveEntryFrame::enter(&runtime.shared, context).unwrap();
    assert_eq!(shared.gate.active_entries(), 2);
    drop(inner);

    assert_eq!(shared.gate.active_entries(), 1);
    assert_eq!(ACTIVE_RUNTIME.with(Cell::get), Rc::as_ptr(&shared));
    assert_eq!(ACTIVE_CONTEXT.with(Cell::get), context.as_ptr());

    drop(outer);
    assert_eq!(shared.gate.active_entries(), 0);
    assert!(ACTIVE_RUNTIME.with(Cell::get).is_null());
    assert!(ACTIVE_CONTEXT.with(Cell::get).is_null());
}

#[test]
fn active_entry_frame_releases_tls_and_admission_during_unwind() {
    let runtime = Runtime::new().unwrap();
    let shared = Rc::clone(&runtime.shared);
    let context = runtime.context.unwrap();

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _entry = ActiveEntryFrame::enter(&runtime.shared, context).unwrap();
        assert_eq!(shared.gate.active_entries(), 1);
        panic!("frame unwind");
    }));

    assert!(result.is_err());
    assert_eq!(shared.gate.active_entries(), 0);
    assert!(ACTIVE_RUNTIME.with(Cell::get).is_null());
    assert!(ACTIVE_CONTEXT.with(Cell::get).is_null());
}

#[test]
fn dropping_foreign_runtime_defers_owned_teardown_until_outer_exit() {
    let mut outer = Runtime::new().unwrap();
    let inner = Runtime::new().unwrap();
    let inner_shared = Rc::clone(&inner.shared);
    let observed_shared = Rc::clone(&inner_shared);

    outer
        .with_context(move |_| {
            drop(inner);
            assert_eq!(observed_shared.gate.state(), HostState::Active);
            assert_eq!(observed_shared.gate.active_entries(), 0);
            assert_eq!(DEFERRED_OWNED_DROPS.with(|drops| drops.borrow().len()), 1);
        })
        .unwrap();

    assert_eq!(inner_shared.gate.state(), HostState::Destroyed);
    assert_eq!(DEFERRED_OWNED_DROPS.with(|drops| drops.borrow().len()), 0);
}

#[test]
fn outer_unwind_drains_deferred_owned_teardown_without_masking_panic() {
    let mut outer = Runtime::new().unwrap();
    let outer_shared = Rc::clone(&outer.shared);
    let inner = Runtime::new().unwrap();
    let inner_shared = Rc::clone(&inner.shared);
    let observed_shared = Rc::clone(&inner_shared);

    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        outer
            .with_context(move |_| {
                drop(inner);
                assert_eq!(observed_shared.gate.state(), HostState::Active);
                assert_eq!(observed_shared.gate.active_entries(), 0);
                assert_eq!(DEFERRED_OWNED_DROPS.with(|drops| drops.borrow().len()), 1);
                panic!("outer entry panic");
            })
            .unwrap();
    }));

    assert_eq!(
        panic
            .as_ref()
            .err()
            .and_then(|payload| payload.downcast_ref::<&str>()),
        Some(&"outer entry panic")
    );
    assert_eq!(outer_shared.gate.active_entries(), 0);
    assert!(ACTIVE_RUNTIME.with(Cell::get).is_null());
    assert!(ACTIVE_CONTEXT.with(Cell::get).is_null());
    assert_eq!(inner_shared.gate.state(), HostState::Destroyed);
    assert_eq!(DEFERRED_OWNED_DROPS.with(|drops| drops.borrow().len()), 0);
}
