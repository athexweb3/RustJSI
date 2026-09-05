// SPDX-License-Identifier: MIT OR Apache-2.0

use super::{ACTIVE_CONTEXT, ACTIVE_RUNTIME, GateError, RuntimeError};
use super::{HostState, Runtime, Shared, Value};
use rustjsi_host::{FinalEntryOutcome, FinalEntryPolicy};
use std::cell::Cell;
use std::rc::{Rc, Weak};

type Snapshot = (HostState, bool);

struct DropSnapshot {
    runtime: Weak<Shared>,
    observed: Rc<Cell<Option<Snapshot>>>,
}

impl Drop for DropSnapshot {
    fn drop(&mut self) {
        let shared = self.runtime.upgrade().unwrap();
        self.observed.set(Some((
            shared.gate.state(),
            shared.gate.cleanup_in_progress(),
        )));
    }
}

#[test]
fn invalidation_separates_cleanup_entry_from_post_engine_destruction() {
    let mut runtime = Runtime::new().unwrap();
    let callback = Rc::new(Cell::new(None));
    let native = Rc::new(Cell::new(None));
    let make_probe = |observed| DropSnapshot {
        runtime: Rc::downgrade(&runtime.shared),
        observed,
    };
    let callback_probe = make_probe(Rc::clone(&callback));
    let native_probe = make_probe(Rc::clone(&native));
    runtime
        .with_context(|cx| {
            cx.install_host_function("cleanupProbe", move |_| {
                let _ = &callback_probe;
                Ok(Value::Undefined)
            })
            .unwrap();
            cx.install_native_state("nativeProbe", native_probe)
                .unwrap();
        })
        .unwrap();
    runtime.invalidate().unwrap();
    assert_eq!(callback.get(), Some((HostState::Draining, true)));
    assert_eq!(native.get(), Some((HostState::Invalid, false)));
    assert!(runtime.context.is_none());
    assert!(!runtime.shared.gate.cleanup_in_progress());
    assert_eq!(runtime.shared.gate.state(), HostState::Destroyed);
    assert_eq!(
        runtime.shared.gate.final_entry_policy(),
        FinalEntryPolicy::Guaranteed
    );
    assert_eq!(
        runtime.shared.gate.final_entry_outcome(),
        Some(FinalEntryOutcome::Completed)
    );
}

#[test]
fn outstanding_cleanup_guard_rejects_teardown_before_engine_release() {
    let mut runtime = Runtime::new().unwrap();
    let shared = Rc::clone(&runtime.shared);
    shared.gate.request_drain();
    let cleanup = shared.gate.try_begin_cleanup().unwrap();
    assert_eq!(
        runtime.invalidate(),
        Err(RuntimeError::Host(GateError::CleanupInProgress))
    );
    assert!(runtime.context.is_some());
    drop(cleanup);
    runtime.invalidate().unwrap();
    assert!(runtime.context.is_none());
}

#[test]
fn cleanup_contains_destructor_panic_without_replacing_outer_runtime_entry() {
    struct PanicDrop {
        shared: Weak<Shared>,
        observed: Rc<Cell<bool>>,
    }
    impl Drop for PanicDrop {
        fn drop(&mut self) {
            self.observed
                .set(self.shared.upgrade().unwrap().gate.cleanup_in_progress());
            panic!("callback capture destructor");
        }
    }
    let mut outer = Runtime::new().unwrap();
    let outer_shared = Rc::clone(&outer.shared);
    let outer_context = outer.context.unwrap();
    let mut inner = Runtime::new().unwrap();
    let observed = Rc::new(Cell::new(false));
    let probe = PanicDrop {
        shared: Rc::downgrade(&inner.shared),
        observed: Rc::clone(&observed),
    };
    inner
        .with_context(|cx| {
            cx.install_host_function("probe", move |_| {
                let _ = &probe;
                Ok(Value::Undefined)
            })
            .unwrap();
        })
        .unwrap();
    outer
        .with_context(|cx| {
            inner.invalidate().unwrap();
            assert!(
                ACTIVE_RUNTIME.with(|active| std::ptr::eq(active.get(), Rc::as_ptr(&outer_shared)))
            );
            assert_eq!(ACTIVE_CONTEXT.with(Cell::get), outer_context.as_ptr());
            let value = cx.eval("42", "outer.js").unwrap();
            assert_eq!(cx.number(&value).unwrap().to_bits(), 42.0_f64.to_bits());
        })
        .unwrap();
    assert!(observed.get());
    assert_eq!(inner.callback_drop_panics(), 1);
    assert!(!inner.shared.gate.cleanup_in_progress());
}
