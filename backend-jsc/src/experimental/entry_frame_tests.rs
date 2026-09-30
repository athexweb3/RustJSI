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
