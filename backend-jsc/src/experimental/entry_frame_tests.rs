// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;

struct DropProbe(Rc<Cell<usize>>);

impl Drop for DropProbe {
    fn drop(&mut self) {
        self.0.set(self.0.get() + 1);
    }
}

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
fn shared_context_group_defers_owned_teardown_until_outer_exit() {
    assert!(take_deferred_owned_drop_drain_observations().is_empty());
    let group = TestContextGroup::new();
    let mut outer = Runtime::new_in_context_group_for_test(group.0).unwrap();
    let inner = Runtime::new_in_context_group_for_test(group.0).unwrap();
    assert_eq!(outer.context_group, group.0);
    assert_eq!(inner.context_group, group.0);
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
    assert_eq!(
        take_deferred_owned_drop_drain_observations(),
        vec![DeferredOwnedDropDrainObservation {
            outer_active_entries: 0,
            active_runtime_present: false,
            active_context_present: false,
        }]
    );
}

#[test]
fn outer_exit_drains_a_burst_of_deferred_owned_runtimes() {
    const DEFERRED_DROPS: usize = 32;

    assert!(take_deferred_owned_drop_drain_observations().is_empty());
    let group = TestContextGroup::new();
    let mut outer = Runtime::new_in_context_group_for_test(group.0).unwrap();
    let inners = (0..DEFERRED_DROPS)
        .map(|_| Runtime::new_in_context_group_for_test(group.0).unwrap())
        .collect::<Vec<_>>();
    let inner_shared = inners
        .iter()
        .map(|runtime| Rc::clone(&runtime.shared))
        .collect::<Vec<_>>();
    let observed_shared = inner_shared.clone();

    outer
        .with_context(move |_| {
            for inner in inners {
                drop(inner);
            }
            assert_eq!(
                DEFERRED_OWNED_DROPS.with(|drops| drops.borrow().len()),
                DEFERRED_DROPS
            );
            for shared in &observed_shared {
                assert_eq!(shared.gate.state(), HostState::Active);
                assert_eq!(shared.gate.active_entries(), 0);
            }
        })
        .unwrap();

    for shared in inner_shared {
        assert_eq!(shared.gate.state(), HostState::Destroyed);
    }
    assert_eq!(DEFERRED_OWNED_DROPS.with(|drops| drops.borrow().len()), 0);
    assert_eq!(
        take_deferred_owned_drop_drain_observations(),
        vec![DeferredOwnedDropDrainObservation {
            outer_active_entries: 0,
            active_runtime_present: false,
            active_context_present: false,
        }]
    );
}

#[test]
fn dropping_independent_owned_runtime_tears_it_down_before_outer_exit() {
    let mut outer = Runtime::new().unwrap();
    let inner = Runtime::new().unwrap();
    let inner_shared = Rc::clone(&inner.shared);

    outer
        .with_context(move |cx| {
            drop(inner);
            assert_eq!(inner_shared.gate.state(), HostState::Destroyed);
            assert_eq!(DEFERRED_OWNED_DROPS.with(|drops| drops.borrow().len()), 0);

            let value = cx.eval("40 + 2", "outer-after-inner-drop.js").unwrap();
            assert_eq!(cx.number(&value).unwrap().to_bits(), 42.0_f64.to_bits());
        })
        .unwrap();
}

#[test]
fn direct_owned_teardown_releases_retained_resources() {
    let callback_drops = Rc::new(Cell::new(0));
    let native_drops = Rc::new(Cell::new(0));
    let mut inner = Runtime::new().unwrap();
    let persistent = inner
        .with_context({
            let callback_probe = DropProbe(Rc::clone(&callback_drops));
            let native_probe = DropProbe(Rc::clone(&native_drops));
            move |cx| {
                cx.install_host_function("teardownProbe", move |_| {
                    let _probe = &callback_probe;
                    Ok(Value::Undefined)
                })
                .unwrap();
                cx.install_native_state("teardownState", native_probe)
                    .unwrap();
                let value = cx.eval("({ answer: 42 })", "teardown-root.js").unwrap();
                cx.persist(&value).unwrap()
            }
        })
        .unwrap();
    let inner_shared = Rc::clone(&inner.shared);
    assert!(
        inner_shared
            .roots
            .borrow()
            .get(persistent.lease.id)
            .is_some()
    );
    assert_eq!(inner_shared.host_functions.borrow().len(), 1);

    let mut outer = Runtime::new().unwrap();
    outer
        .with_context(move |_| {
            drop(inner);
            assert_eq!(inner_shared.gate.state(), HostState::Destroyed);
            assert!(
                inner_shared
                    .roots
                    .borrow()
                    .get(persistent.lease.id)
                    .is_none()
            );
            assert!(inner_shared.host_functions.borrow().is_empty());
            assert_eq!(callback_drops.get(), 1);
            assert_eq!(native_drops.get(), 1);
            drop(persistent);
        })
        .unwrap();
}

#[test]
fn shared_context_group_deferred_teardown_releases_retained_resources() {
    let callback_drops = Rc::new(Cell::new(0));
    let native_drops = Rc::new(Cell::new(0));
    let group = TestContextGroup::new();
    let mut inner = Runtime::new_in_context_group_for_test(group.0).unwrap();
    let persistent = inner
        .with_context({
            let callback_probe = DropProbe(Rc::clone(&callback_drops));
            let native_probe = DropProbe(Rc::clone(&native_drops));
            move |cx| {
                cx.install_host_function("deferredTeardownProbe", move |_| {
                    let _probe = &callback_probe;
                    Ok(Value::Undefined)
                })
                .unwrap();
                cx.install_native_state("deferredTeardownState", native_probe)
                    .unwrap();
                let value = cx
                    .eval("({ answer: 42 })", "deferred-teardown-root.js")
                    .unwrap();
                cx.persist(&value).unwrap()
            }
        })
        .unwrap();
    let persistent_id = persistent.lease.id;
    let inner_shared = Rc::clone(&inner.shared);
    let observed_shared = Rc::clone(&inner_shared);
    let observed_callback_drops = Rc::clone(&callback_drops);
    let observed_native_drops = Rc::clone(&native_drops);
    assert!(inner_shared.roots.borrow().get(persistent_id).is_some());
    assert_eq!(inner_shared.host_functions.borrow().len(), 1);

    let mut outer = Runtime::new_in_context_group_for_test(group.0).unwrap();
    outer
        .with_context(move |_| {
            drop(inner);
            assert_eq!(observed_shared.gate.state(), HostState::Active);
            assert!(observed_shared.roots.borrow().get(persistent_id).is_some());
            assert_eq!(observed_shared.host_functions.borrow().len(), 1);
            assert_eq!(observed_callback_drops.get(), 0);
            assert_eq!(observed_native_drops.get(), 0);
            assert_eq!(DEFERRED_OWNED_DROPS.with(|drops| drops.borrow().len()), 1);
            drop(persistent);
        })
        .unwrap();

    assert_eq!(inner_shared.gate.state(), HostState::Destroyed);
    assert!(inner_shared.roots.borrow().get(persistent_id).is_none());
    assert!(inner_shared.host_functions.borrow().is_empty());
    assert_eq!(callback_drops.get(), 1);
    assert_eq!(native_drops.get(), 1);
    assert_eq!(DEFERRED_OWNED_DROPS.with(|drops| drops.borrow().len()), 0);
}

#[test]
fn independent_owned_teardown_during_outer_unwind_preserves_the_panic() {
    let mut outer = Runtime::new().unwrap();
    let inner = Runtime::new().unwrap();
    let inner_shared = Rc::clone(&inner.shared);

    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        outer
            .with_context(move |_| {
                drop(inner);
                assert_eq!(inner_shared.gate.state(), HostState::Destroyed);
                assert_eq!(DEFERRED_OWNED_DROPS.with(|drops| drops.borrow().len()), 0);
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
    assert!(ACTIVE_RUNTIME.with(Cell::get).is_null());
    assert!(ACTIVE_CONTEXT.with(Cell::get).is_null());
}

#[test]
fn backend_entry_tears_down_independent_owned_runtime_before_outer_exit() {
    let mut outer = Runtime::new().unwrap();
    let inner = Runtime::new().unwrap();
    let inner_shared = Rc::clone(&inner.shared);

    outer
        .with_backend(move |_| {
            drop(inner);
            assert_eq!(inner_shared.gate.state(), HostState::Destroyed);
            assert_eq!(DEFERRED_OWNED_DROPS.with(|drops| drops.borrow().len()), 0);
        })
        .unwrap();
}

#[test]
fn backend_entry_drains_deferred_owned_teardown_at_outer_exit() {
    let mut outer = Runtime::new().unwrap();
    let mut inner = Runtime::new().unwrap();
    inner.context_group = outer.context_group;
    let inner_shared = Rc::clone(&inner.shared);
    let observed_shared = Rc::clone(&inner_shared);

    outer
        .with_backend(move |_| {
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
fn nested_attachment_entry_does_not_drain_deferred_owned_teardown_early() {
    let mut identity = RuntimeIdentity::allocate().unwrap();
    let mut attachment = Attachment::new(&mut identity, FinalEntryPolicy::Guaranteed).unwrap();
    // SAFETY: This test owns the JSC global context and releases it after the
    // attachment's legal final entry.
    let context = unsafe { sys::global_context_create(std::ptr::null_mut()) };
    let context = NonNull::new(context).expect("JSC test context");
    let mut inner = Runtime::new().unwrap();
    // SAFETY: `context` is live for this test and the entry frame owns it.
    let attachment_group = unsafe { sys::context_get_group(context.as_ptr()) };
    inner.context_group = NonNull::new(attachment_group).expect("JSC context group");
    let inner_shared = Rc::clone(&inner.shared);

    let outer = ActiveEntryFrame::enter(&attachment.shared, context).unwrap();
    let nested = ActiveEntryFrame::enter(&attachment.shared, context).unwrap();
    assert_eq!(attachment.shared.gate.active_entries(), 2);

    drop(inner);
    assert_eq!(inner_shared.gate.state(), HostState::Active);
    assert_eq!(DEFERRED_OWNED_DROPS.with(|drops| drops.borrow().len()), 1);

    drop(nested);
    assert_eq!(attachment.shared.gate.active_entries(), 1);
    assert_eq!(inner_shared.gate.state(), HostState::Active);
    assert_eq!(DEFERRED_OWNED_DROPS.with(|drops| drops.borrow().len()), 1);

    drop(outer);
    assert_eq!(attachment.shared.gate.active_entries(), 0);
    assert_eq!(inner_shared.gate.state(), HostState::Destroyed);
    assert_eq!(DEFERRED_OWNED_DROPS.with(|drops| drops.borrow().len()), 0);

    // SAFETY: No entry remains, `attachment` is bound to this live context, and
    // this call performs its required final cleanup before this test releases it.
    unsafe {
        let _ = attachment
            .detach_with_context(context.as_ptr().cast())
            .unwrap();
        sys::global_context_release(context.as_ptr());
    }
}

#[test]
fn outer_unwind_drains_deferred_owned_teardown_without_masking_panic() {
    let mut outer = Runtime::new().unwrap();
    let outer_shared = Rc::clone(&outer.shared);
    let mut inner = Runtime::new().unwrap();
    inner.context_group = outer.context_group;
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
