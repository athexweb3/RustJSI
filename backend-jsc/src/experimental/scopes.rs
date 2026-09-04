// SPDX-License-Identifier: MIT OR Apache-2.0

use super::{Context, LocalRoots, RuntimeError};
use std::cell::RefCell;
use std::marker::PhantomData;

const MAX_SCOPE_DEPTH: u32 = 64;

impl Context<'_> {
    /// Runs a child local-root scope inside the current host entry.
    ///
    /// Child locals are released on return or unwind without releasing parent
    /// roots. Return owned Rust data or explicitly persist a value to keep it.
    /// This does not admit another host entry or drain pending persistent roots
    /// and native finalizers. Keep batches small to limit local retention; this
    /// API does not impose a root-count or JavaScript heap quota.
    ///
    /// ```
    /// use rustjsi_backend_jsc::Runtime;
    /// let mut runtime = Runtime::new().unwrap();
    /// runtime.with_context(|cx| {
    ///     for _ in 0..100 {
    ///         cx.with_scope(|child| {
    ///             let _ = child.eval("({ answer: 42 })", "batch.js").unwrap();
    ///         }).unwrap(); // releases this batch's local roots
    ///     }
    /// }).unwrap();
    /// ```
    ///
    /// ```compile_fail
    /// use rustjsi_backend_jsc::Runtime;
    /// let mut runtime = Runtime::new().unwrap();
    /// runtime.with_context(|cx| {
    ///     let escaped = cx.with_scope(|child| child.eval("({})", "escape.js").unwrap()).unwrap();
    ///     cx.string(&escaped).unwrap();
    /// }).unwrap();
    /// ```
    ///
    /// ```compile_fail
    /// use rustjsi_backend_jsc::Runtime;
    /// let mut runtime = Runtime::new().unwrap();
    /// runtime.with_context(|cx| {
    ///     let mut escaped = None;
    ///     cx.with_scope(|child| {
    ///         escaped = Some(child.eval("({})", "capture.js").unwrap());
    ///     }).unwrap();
    ///     drop(escaped);
    /// }).unwrap();
    /// ```
    ///
    /// # Errors
    ///
    /// Rejects inactive/wrong-thread access or more than 64 nested child scopes
    /// before invoking `operation`. The root Context has depth zero.
    pub fn with_scope<R>(
        &mut self,
        operation: impl for<'scope> FnOnce(&mut Context<'scope>) -> R,
    ) -> Result<R, RuntimeError> {
        self.shared.ensure_active()?;
        if self.scope_depth >= MAX_SCOPE_DEPTH {
            return Err(RuntimeError::ScopeDepthExceeded);
        }
        let mut child = Context {
            shared: self.shared,
            raw: self.raw,
            local_roots: RefCell::new(LocalRoots::new()),
            scope_depth: self.scope_depth + 1,
            _affine: PhantomData,
        };
        Ok(operation(&mut child))
    }
}

#[cfg(test)]
mod tests {
    use super::{Context, MAX_SCOPE_DEPTH, RuntimeError};
    use crate::experimental::{ACTIVE_RUNTIME, JsError, Runtime};
    use std::rc::Rc;

    #[test]
    fn batches_release_child_roots_without_releasing_parent_values() {
        let mut runtime = Runtime::new().unwrap();
        let shared = Rc::clone(&runtime.shared);
        runtime
            .with_context(|cx| {
                let parent = cx.eval("'parent'", "parent.js").unwrap();
                for _ in 0..100 {
                    cx.with_scope(|child| {
                        for _ in 0..32 {
                            let _ = child.eval("({})", "batch.js").unwrap();
                        }
                        assert_eq!(shared.context_local_roots.get(), 33);
                        assert_eq!(child.string(&parent).unwrap(), "parent");
                        assert_eq!(shared.gate.active_entries(), 1);
                        assert!(
                            ACTIVE_RUNTIME
                                .with(|active| std::ptr::eq(active.get(), Rc::as_ptr(&shared)))
                        );
                    })
                    .unwrap();
                    assert_eq!(shared.context_local_roots.get(), 1);
                    cx.collect_garbage().unwrap();
                    assert_eq!(cx.string(&parent).unwrap(), "parent");
                }
            })
            .unwrap();
        assert_eq!(shared.context_local_roots.get(), 0);
    }

    #[test]
    fn persistent_promotion_crosses_child_and_host_boundaries() {
        let mut runtime = Runtime::new().unwrap();
        let shared = Rc::clone(&runtime.shared);
        let persistent = runtime
            .with_context(|cx| {
                let root = cx
                    .with_scope(|child| {
                        let value = child.eval("'retained'", "promote.js").unwrap();
                        child.persist(&value).unwrap()
                    })
                    .unwrap();
                assert_eq!(shared.context_local_roots.get(), 0);
                cx.collect_garbage().unwrap();
                let value = cx.resolve(&root).unwrap();
                assert_eq!(cx.string(&value).unwrap(), "retained");
                root
            })
            .unwrap();
        runtime
            .with_context(|cx| {
                let value = cx.resolve(&persistent).unwrap();
                assert_eq!(cx.string(&value).unwrap(), "retained");
            })
            .unwrap();
    }

    #[test]
    fn nested_unwind_releases_only_child_roots() {
        let mut runtime = Runtime::new().unwrap();
        let shared = Rc::clone(&runtime.shared);
        runtime
            .with_context(|cx| {
                let parent = cx.eval("'parent'", "parent.js").unwrap();
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    cx.with_scope(|child| {
                        let _ = child.eval("'child'", "child.js").unwrap();
                        child
                            .with_scope(|grandchild| {
                                for _ in 0..32 {
                                    let _ = grandchild.eval("({})", "grandchild.js").unwrap();
                                }
                                assert_eq!(shared.context_local_roots.get(), 34);
                                panic!("nested scope panic");
                            })
                            .unwrap();
                    })
                    .unwrap();
                }));
                assert!(result.is_err());
                assert_eq!(shared.context_local_roots.get(), 1);
                assert_eq!(shared.gate.active_entries(), 1);
                cx.collect_garbage().unwrap();
                assert_eq!(cx.string(&parent).unwrap(), "parent");
                cx.with_scope(|_| ()).unwrap();
            })
            .unwrap();
        assert_eq!(shared.context_local_roots.get(), 0);
    }

    #[test]
    fn javascript_error_return_releases_child_roots() {
        let mut runtime = Runtime::new().unwrap();
        let shared = Rc::clone(&runtime.shared);
        runtime
            .with_context(|cx| {
                let result: Result<(), JsError> = cx
                    .with_scope(|child| {
                        let _ = child.eval("({})", "child.js")?;
                        let _ = child.eval("throw new Error('failed batch')", "failure.js")?;
                        Ok(())
                    })
                    .unwrap();
                assert!(matches!(result, Err(JsError::Exception(_))));
                assert_eq!(shared.context_local_roots.get(), 0);
                cx.with_scope(|_| ()).unwrap();
            })
            .unwrap();
    }

    #[test]
    fn scope_depth_limit_is_independent_of_host_admission() {
        fn descend(cx: &mut Context<'_>) -> Result<(), RuntimeError> {
            let _ = cx.eval("({})", "depth.js").unwrap();
            assert_eq!(cx.shared.gate.active_entries(), 1);
            assert_eq!(
                cx.shared.context_local_roots.get(),
                cx.scope_depth as usize + 1
            );
            if cx.scope_depth == MAX_SCOPE_DEPTH {
                return cx.with_scope(|_| panic!("scope depth limit was bypassed"));
            }
            cx.with_scope(descend)?
        }

        let mut runtime = Runtime::new().unwrap();
        let shared = Rc::clone(&runtime.shared);
        assert_eq!(
            runtime.with_context(descend).unwrap(),
            Err(RuntimeError::ScopeDepthExceeded)
        );
        assert_eq!(shared.context_local_roots.get(), 0);
        assert_eq!(shared.gate.active_entries(), 0);
        runtime
            .with_context(|cx| cx.with_scope(|_| ()).unwrap())
            .unwrap();
    }

    #[test]
    fn draining_rejects_child_scope_before_user_code() {
        let mut runtime = Runtime::new().unwrap();
        let shared = Rc::clone(&runtime.shared);
        runtime
            .with_context(|cx| {
                shared.gate.request_drain();
                assert_eq!(
                    cx.with_scope(|_| panic!("draining scope ran")).unwrap_err(),
                    RuntimeError::Invalidated
                );
            })
            .unwrap();
        runtime.invalidate().unwrap();
    }

    #[test]
    fn scopes_do_not_flush_persistent_releases_or_undo_native_registration() {
        let mut runtime = Runtime::new().unwrap();
        let shared = Rc::clone(&runtime.shared);
        runtime
            .with_context(|cx| {
                let object = cx
                    .with_scope(|child| {
                        let value = child.eval("({})", "scope.js").unwrap();
                        let root = child.persist(&value).unwrap();
                        drop(root);
                        child
                            .install_native_state("registeredState", 42_u32)
                            .unwrap()
                    })
                    .unwrap();
                assert_eq!(shared.context_local_roots.get(), 0);
                assert!(shared.roots.borrow().pending_head.is_some());
                assert_eq!(cx.with_native_state(&object, |state| *state).unwrap(), 42);
            })
            .unwrap();
        assert!(shared.roots.borrow().pending_head.is_none());
    }
}
