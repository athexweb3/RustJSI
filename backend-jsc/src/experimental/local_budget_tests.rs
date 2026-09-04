// SPDX-License-Identifier: MIT OR Apache-2.0

use super::{JsError, RootLimits, Runtime, RuntimeError, Value};
use rustjsi_backend::{
    BackendBase, BackendError, BackendScope, OwnedExternalBufferScope, OwnershipTransferError,
    RootScope,
};
use std::{cell::Cell, rc::Rc};

fn limited(local_roots: usize) -> Runtime {
    Runtime::new_with_root_limits(RootLimits {
        local_roots,
        ..RootLimits::default()
    })
    .unwrap()
}

#[test]
fn full_local_budget_rejects_before_javascript_side_effects() {
    let mut runtime = Runtime::new().unwrap();
    runtime
        .with_context(|cx| {
            for _ in 0..4096 {
                let _ = cx.eval("({})", "fill.js").unwrap();
            }
            assert!(
                cx.eval("globalThis.quotaBypassed = true; ({})", "reject.js")
                    .is_err()
            );
        })
        .unwrap();
    runtime
        .with_context(|cx| {
            let value = cx
                .eval("globalThis.quotaBypassed === undefined", "check.js")
                .unwrap();
            assert!(cx.boolean(&value).unwrap());
        })
        .unwrap();
}

#[test]
fn child_cleanup_and_unwind_return_only_their_local_capacity() {
    let mut runtime = limited(2);
    runtime
        .with_context(|cx| {
            let parent = cx.eval("'parent'", "parent.js").unwrap();
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                cx.with_scope(|child| {
                    let _ = child.eval("({})", "child.js").unwrap();
                    assert_eq!(
                        child.eval("42", "full.js").unwrap_err(),
                        JsError::Runtime(RuntimeError::LocalRootLimitReached)
                    );
                    panic!("child cleanup");
                })
                .unwrap();
            }));
            assert!(result.is_err());
            cx.collect_garbage().unwrap();
            assert_eq!(cx.string(&parent).unwrap(), "parent");
            for _ in 0..100 {
                cx.with_scope(|child| {
                    let _ = child.eval("({})", "reuse.js").unwrap();
                })
                .unwrap();
            }
        })
        .unwrap();
    runtime
        .with_backend(|backend| {
            let scope = backend.open_scope().unwrap();
            scope.string("one").unwrap();
            scope.string("two").unwrap();
            assert!(scope.string("three").is_err());
        })
        .unwrap();
}

#[test]
fn quota_rejection_never_invokes_the_host_callback() {
    let mut runtime = limited(1);
    let calls = Rc::new(Cell::new(0));
    runtime
        .with_context(|cx| {
            let count = Rc::clone(&calls);
            let function = cx
                .install_host_function("counted", move |_| {
                    count.set(count.get() + 1);
                    Ok(Value::Number(42.0))
                })
                .unwrap();
            let _ = cx.eval("({})", "fill.js").unwrap();
            assert_eq!(
                cx.call(&function, &[]).unwrap_err(),
                JsError::Runtime(RuntimeError::LocalRootLimitReached)
            );
        })
        .unwrap();
    assert_eq!(calls.get(), 0);
}

#[test]
fn scalar_and_exception_paths_refund_context_reservations() {
    let mut runtime = limited(1);
    runtime
        .with_context(|cx| {
            for _ in 0..100 {
                let value = cx.eval("42", "scalar.js").unwrap();
                assert_eq!(cx.number(&value).unwrap().to_bits(), 42.0_f64.to_bits());
                assert!(matches!(
                    cx.eval("throw new Error('expected')", "throw.js"),
                    Err(JsError::Exception(_))
                ));
            }
            let local = cx.eval("({})", "fill.js").unwrap();
            let root = cx.persist(&local).unwrap();
            assert!(cx.resolve(&root).is_err());
            root
        })
        .unwrap();
    runtime
        .with_backend(|backend| {
            let scope = backend.open_scope().unwrap();
            assert!(scope.evaluate("throw 1", "throw.js").is_err());
            scope.string("capacity recovered").unwrap();
        })
        .unwrap();
}

#[test]
fn external_local_rejection_returns_exact_owner_before_transfer() {
    let mut runtime = limited(0);
    let shared = Rc::clone(&runtime.shared);
    runtime
        .with_backend(|backend| {
            let scope = backend.open_scope().unwrap();
            scope.number(42.0).unwrap();
            assert!(scope.string("no slot").is_err());
            assert!(
                scope
                    .evaluate("globalThis.forbidden = true", "reject.js")
                    .is_err()
            );
            let owner = vec![1, 2, 3].into_boxed_slice();
            let pointer = owner.as_ptr();
            match scope.externalize(owner) {
                Err(OwnershipTransferError::Rejected { owner, error }) => {
                    assert_eq!(owner.as_ptr(), pointer);
                    assert_eq!(&*owner, &[1, 2, 3]);
                    assert_eq!(
                        error,
                        BackendError::Failure("local result root limit reached")
                    );
                }
                other => panic!("unexpected transfer result: {other:?}"),
            }
            assert_eq!(shared.external_buffers.live_allocations(), 0);
            assert_eq!(shared.external_buffers.live_bytes(), 0);
        })
        .unwrap();
}

#[test]
fn common_resolution_rejection_preserves_the_persistent_root() {
    let mut runtime = limited(0);
    runtime
        .with_backend(|backend| {
            let scope = backend.open_scope().unwrap();
            let value = scope.number(42.0).unwrap();
            let root = scope.persist(value).unwrap();
            assert!(scope.resolve(root).is_err());
            scope.release(root).unwrap();
        })
        .unwrap();
}
