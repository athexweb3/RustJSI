// SPDX-License-Identifier: MIT OR Apache-2.0

use super::{JsError, RootLimits, Runtime, RuntimeError, Shared, Value};
use std::cell::Cell;
use std::rc::{Rc, Weak};

struct DropProbe {
    shared: Weak<Shared>,
    drops: Rc<Cell<usize>>,
}

impl Drop for DropProbe {
    fn drop(&mut self) {
        assert!(
            self.shared
                .upgrade()
                .unwrap()
                .host_functions
                .try_borrow_mut()
                .is_ok()
        );
        self.drops.set(self.drops.get() + 1);
    }
}

#[test]
fn full_callback_registry_rejects_before_publication() {
    let mut runtime = Runtime::new().unwrap();
    let shared = Rc::clone(&runtime.shared);
    let drops = Rc::new(Cell::new(0));
    runtime
        .with_context(|cx| {
            cx.eval(
                "globalThis.published = 0; Object.defineProperty(globalThis, 'overflow', { set(_) { ++published; } });",
                "setter.js",
            )
            .unwrap();
            for _ in 0..4096 {
                let probe = DropProbe {
                    shared: Rc::downgrade(&shared),
                    drops: Rc::clone(&drops),
                };
                cx.install_host_function("retained", move |_| {
                    let _ = &probe;
                    Ok(Value::Undefined)
                })
                    .unwrap();
            }
            assert_eq!(drops.get(), 0);
            let rejected = DropProbe {
                shared: Rc::downgrade(&shared),
                drops: Rc::clone(&drops),
            };
            assert!(matches!(
                cx.install_host_function("overflow", move |_| {
                    let _ = &rejected;
                    Ok(Value::Undefined)
                }),
                Err(JsError::Runtime(RuntimeError::HostFunctionLimitReached))
            ));
            assert_eq!(drops.get(), 1);
            assert_eq!(shared.host_functions.borrow().len(), 4096);
            let untouched = cx.eval("published === 0", "check.js").unwrap();
            assert!(cx.boolean(&untouched).unwrap());
        })
        .unwrap();
    runtime
        .with_context(|cx| {
            cx.with_scope(|child| {
                assert!(matches!(
                    child.install_host_function("overflow", |_| Ok(Value::Undefined)),
                    Err(JsError::Runtime(RuntimeError::HostFunctionLimitReached))
                ));
                let value = child
                    .eval("retained() === undefined", "existing.js")
                    .unwrap();
                assert!(child.boolean(&value).unwrap());
            })
            .unwrap();
        })
        .unwrap();
    Runtime::new()
        .unwrap()
        .with_context(|cx| {
            cx.install_host_function("independent", |_| Ok(Value::Undefined))
                .unwrap();
        })
        .unwrap();
    runtime.invalidate().unwrap();
    assert_eq!(drops.get(), 4097);
    assert!(shared.host_functions.borrow().is_empty());
}

#[test]
fn failed_publication_returns_callback_capacity() {
    let mut runtime = Runtime::new().unwrap();
    let shared = Rc::clone(&runtime.shared);
    let drops = Rc::new(Cell::new(0));
    runtime.with_context(|cx| {
        cx.eval(
            "Object.defineProperty(globalThis, 'reject', { set(value) { globalThis.saved = value; throw new Error('rejected'); } });",
            "setter.js",
        ).unwrap();
        for _ in 0..4095 {
            cx.install_host_function("retained", |_| Ok(Value::Undefined)).unwrap();
        }
        for _ in 0..100 {
            let probe = DropProbe {
                shared: Rc::downgrade(&shared),
                drops: Rc::clone(&drops),
            };
            assert!(matches!(cx.install_host_function("reject", move |_| {
                let _ = &probe;
                Ok(Value::Undefined)
            }), Err(JsError::Exception(_))));
            assert_eq!(shared.host_functions.borrow().len(), 4095);
        }
        assert_eq!(drops.get(), 100);
        cx.install_host_function("last", |_| Ok(Value::Undefined)).unwrap();
        assert_eq!(shared.host_functions.borrow().len(), 4096);
        let stale = cx.eval("saved()", "stale.js").unwrap_err();
        assert!(stale.to_string().contains("registration is stale"));
        assert!(matches!(
            cx.install_host_function("overflow", |_| Ok(Value::Undefined)),
            Err(JsError::Runtime(RuntimeError::HostFunctionLimitReached))
        ));
    }).unwrap();
    runtime.invalidate().unwrap();
    assert_eq!(drops.get(), 100);
}

#[test]
fn callback_retention_is_independent_of_root_budgets() {
    let mut runtime = Runtime::new_with_root_limits(RootLimits {
        persistent_slots: 0,
        local_roots: 0,
    })
    .unwrap();
    let shared = Rc::clone(&runtime.shared);
    let drops = Rc::new(Cell::new(0));
    runtime
        .with_context(|cx| {
            let probe = DropProbe {
                shared: Rc::downgrade(&shared),
                drops: Rc::clone(&drops),
            };
            let function = cx
                .install_host_function("retained", move |_| {
                    let _ = &probe;
                    Ok(Value::Undefined)
                })
                .unwrap();
            drop(function);
            assert_eq!(drops.get(), 0);
            assert_eq!(shared.host_functions.borrow().len(), 1);
        })
        .unwrap();
    runtime.invalidate().unwrap();
    assert_eq!(drops.get(), 1);
}
