// SPDX-License-Identifier: MIT OR Apache-2.0

use super::{HostState, Runtime, Shared, Value};
use std::cell::Cell;
use std::rc::{Rc, Weak};

type Snapshot = (HostState, bool, bool);

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
            shared.context.get().is_some(),
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
    assert_eq!(callback.get(), Some((HostState::Draining, true, true)));
    assert_eq!(native.get(), Some((HostState::Invalid, false, false)));
    assert!(!runtime.shared.gate.cleanup_in_progress());
    assert_eq!(runtime.shared.gate.state(), HostState::Destroyed);
}
