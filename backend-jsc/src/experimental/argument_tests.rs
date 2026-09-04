// SPDX-License-Identifier: MIT OR Apache-2.0

use super::{Runtime, Value};
use std::cell::Cell;
use std::rc::Rc;

#[test]
fn heap_arguments_have_call_scoped_roots() {
    let mut runtime = Runtime::new().unwrap();
    let observed = Rc::new(Cell::new(0));
    let shared = Rc::clone(&runtime.shared);
    runtime.with_context(|cx| {
        let snapshot = Rc::clone(&observed);
        let weak = Rc::downgrade(&shared);
        let function = cx.install_host_function("inspectArguments", move |_| {
            snapshot.set(weak.upgrade().unwrap().argument_roots.get());
            Ok(Value::Undefined)
        }).unwrap();
        let arguments: Vec<_> = (0..128).map(|i| Value::String(format!("argument-{i}"))).collect();
        cx.call(&function, &arguments).unwrap();
        assert_eq!(observed.get(), 128);
        assert_eq!(shared.argument_roots.get(), 0);
    }).unwrap();
}
