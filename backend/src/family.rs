// SPDX-License-Identifier: MIT OR Apache-2.0

use crate::{BackendBase, BackendError, BackendScope};

/// Entry and scope types for one source-linked backend implementation.
///
/// Consumers can bound `Scope<'scope>` capabilities without quantifying over
/// scope lifetimes longer than a borrowed backend's entry. The family holds no
/// runtime state and grants no engine-entry authority.
///
/// A scope cannot escape its operation:
///
/// ```compile_fail
/// use rustjsi_backend::BackendFamily;
/// fn escape<F: BackendFamily>(backend: &mut F::Backend<'_>) {
///     let _scope = F::with_scope(backend, |scope| scope);
/// }
/// ```
///
/// Neither can a local value:
///
/// ```compile_fail
/// use rustjsi_backend::{BackendFamily, BackendScope};
/// fn escape<F: BackendFamily>(backend: &mut F::Backend<'_>) {
///     let _value = F::with_scope(backend, |scope| scope.string("local").unwrap());
/// }
/// ```
pub trait BackendFamily {
    /// Backend adapter borrowed during an existing legal entry.
    type Backend<'entry>: BackendBase;

    /// Scope with the backend borrow shortened to the scope's own lifetime.
    type Scope<'scope>: BackendScope<Backend = Self::Backend<'scope>>;

    /// Opens a scope on the supplied backend and passes it to `operation`.
    ///
    /// Implementations must preserve the supplied instance's identity, state,
    /// capabilities and scope cleanup. They must not enter another runtime or
    /// extend the host's lifetime. The operation may return owned data or a
    /// persistent ID, but cannot return a borrowed scope or local value.
    ///
    /// # Errors
    ///
    /// Returns a contained scope-creation failure without running `operation`.
    fn with_scope<R>(
        backend: &mut Self::Backend<'_>,
        operation: impl for<'scope> FnOnce(Self::Scope<'scope>) -> R,
    ) -> Result<R, BackendError>;
}
