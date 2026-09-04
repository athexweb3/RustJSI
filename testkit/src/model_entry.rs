// SPDX-License-Identifier: MIT OR Apache-2.0

use crate::{ModelBackend, ModelBufferView, ModelRoot, ModelScope, ModelValue};
use rustjsi_backend::{
    BackendBase, BackendError, BackendFamily, BackendManifest, BackendScope, BorrowedBufferScope,
    OwnedExternalBufferScope, OwnershipTransferError, RootBackend, RootScope, ValueKind,
};
use std::marker::PhantomData;
use std::rc::Rc;

/// Type family for borrowed deterministic-model entries and scopes.
#[derive(Debug)]
pub enum ModelBackendFamily {}

impl BackendFamily for ModelBackendFamily {
    type Backend<'entry> = ModelEntry<'entry>;
    type Scope<'scope> = ModelEntryScope<'scope, 'scope>;

    fn with_scope<R>(
        backend: &mut ModelEntry<'_>,
        operation: impl for<'scope> FnOnce(Self::Scope<'scope>) -> R,
    ) -> Result<R, BackendError> {
        Ok(operation(backend.open_scope()?))
    }
}

/// A borrowed adapter for testing entry-confined backend access.
///
/// The owning model retains roots, evaluation outcomes and buffer accounting.
/// This adapter adds no lifecycle state, scheduler or engine-entry authority.
/// It is available only through [`ModelBackend::with_entry`].
///
/// ```compile_fail
/// use rustjsi_testkit::ModelEntry;
/// fn require_send<T: Send>() {}
/// require_send::<ModelEntry<'static>>();
/// ```
///
/// ```compile_fail
/// use rustjsi_testkit::ModelEntry;
/// fn require_sync<T: Sync>() {}
/// require_sync::<ModelEntry<'static>>();
/// ```
#[derive(Debug)]
pub struct ModelEntry<'entry> {
    backend: &'entry mut ModelBackend,
    affinity: PhantomData<Rc<()>>,
}

/// A local scope borrowing a [`ModelEntry`].
#[derive(Debug)]
pub struct ModelEntryScope<'scope, 'entry> {
    inner: ModelScope<'scope>,
    entry: PhantomData<&'entry mut ModelBackend>,
}

impl ModelBackend {
    /// Lends a thread-affine adapter without transferring model ownership.
    ///
    /// Root IDs may cross entries. Adapter references and scoped values cannot.
    /// A host fixture must provide any admission or invalidation policy itself.
    ///
    /// ```compile_fail
    /// use rustjsi_testkit::ModelBackend;
    /// let mut model = ModelBackend::new();
    /// let escaped = model.with_entry(|entry| entry);
    /// drop(escaped);
    /// ```
    ///
    /// ```compile_fail
    /// use rustjsi_backend::{BackendBase, BackendScope};
    /// use rustjsi_testkit::ModelBackend;
    /// let mut model = ModelBackend::new();
    /// let escaped = model.with_entry(|entry| {
    ///     let scope = entry.open_scope().unwrap();
    ///     scope.string("local").unwrap()
    /// });
    /// drop(escaped);
    /// ```
    pub fn with_entry<R>(
        &mut self,
        operation: impl for<'entry> FnOnce(&mut ModelEntry<'entry>) -> R,
    ) -> R {
        operation(&mut ModelEntry {
            backend: self,
            affinity: PhantomData,
        })
    }
}

impl<'entry> BackendBase for ModelEntry<'entry> {
    type Scope<'scope>
        = ModelEntryScope<'scope, 'entry>
    where
        Self: 'scope;

    fn manifest(&self) -> BackendManifest {
        self.backend.manifest()
    }

    fn open_scope(&mut self) -> Result<Self::Scope<'_>, BackendError> {
        Ok(ModelEntryScope {
            inner: self.backend.open_scope()?,
            entry: PhantomData,
        })
    }
}

impl RootBackend for ModelEntry<'_> {
    type Root = ModelRoot;
}

impl<'entry> BackendScope for ModelEntryScope<'_, 'entry> {
    type Backend = ModelEntry<'entry>;
    type Value<'value>
        = ModelValue<'value>
    where
        Self: 'value;

    fn undefined(&self) -> Result<Self::Value<'_>, BackendError> {
        self.inner.undefined()
    }

    fn null(&self) -> Result<Self::Value<'_>, BackendError> {
        self.inner.null()
    }

    fn boolean(&self, value: bool) -> Result<Self::Value<'_>, BackendError> {
        self.inner.boolean(value)
    }

    fn number(&self, value: f64) -> Result<Self::Value<'_>, BackendError> {
        self.inner.number(value)
    }

    fn string(&self, value: &str) -> Result<Self::Value<'_>, BackendError> {
        self.inner.string(value)
    }

    fn evaluate(&self, source: &str, source_url: &str) -> Result<Self::Value<'_>, BackendError> {
        self.inner.evaluate(source, source_url)
    }

    fn kind<'value>(&'value self, value: Self::Value<'value>) -> Result<ValueKind, BackendError> {
        self.inner.kind(value)
    }

    fn as_boolean<'value>(&'value self, value: Self::Value<'value>) -> Result<bool, BackendError> {
        self.inner.as_boolean(value)
    }

    fn as_number<'value>(&'value self, value: Self::Value<'value>) -> Result<f64, BackendError> {
        self.inner.as_number(value)
    }

    fn to_string<'value>(&'value self, value: Self::Value<'value>) -> Result<String, BackendError> {
        self.inner.to_string(value)
    }
}

impl RootScope for ModelEntryScope<'_, '_> {
    fn persist<'value>(
        &'value self,
        value: Self::Value<'value>,
    ) -> Result<ModelRoot, BackendError> {
        self.inner.persist(value)
    }

    fn resolve(&self, root: ModelRoot) -> Result<Self::Value<'_>, BackendError> {
        self.inner.resolve(root)
    }

    fn release(&self, root: ModelRoot) -> Result<(), BackendError> {
        self.inner.release(root)
    }
}

impl OwnedExternalBufferScope for ModelEntryScope<'_, '_> {
    fn externalize(
        &self,
        owner: Box<[u8]>,
    ) -> Result<Self::Value<'_>, OwnershipTransferError<Box<[u8]>>> {
        self.inner.externalize(owner)
    }
}

impl BorrowedBufferScope for ModelEntryScope<'_, '_> {
    type BufferView<'view>
        = ModelBufferView<'view>
    where
        Self: 'view;

    fn buffer_bytes<'view>(
        &'view self,
        value: Self::Value<'view>,
    ) -> Result<Self::BufferView<'view>, BackendError> {
        self.inner.buffer_bytes(value)
    }
}
