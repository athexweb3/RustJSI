// SPDX-License-Identifier: MIT OR Apache-2.0

use rustjsi_backend::{
    BACKEND_CONTRACT_VERSION, BackendBase, BackendError, BackendException, BackendManifest,
    BackendScope, Capability, CapabilitySet, OwnedExternalBufferScope, OwnershipTransferError,
    RootScope, ValueKind,
};
use std::cell::{Cell, Ref, RefCell};
use std::collections::{HashSet, VecDeque};
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_BACKEND_ID: AtomicU64 = AtomicU64::new(1);

/// A primitive value produced by the deterministic backend.
#[derive(Clone, Debug, PartialEq)]
pub enum Primitive {
    /// JavaScript `undefined`.
    Undefined,
    /// JavaScript `null`.
    Null,
    /// A Boolean.
    Boolean(bool),
    /// A number.
    Number(f64),
    /// A string.
    String(String),
}

/// A pre-programmed result for one deterministic evaluation.
#[derive(Clone, Debug, PartialEq)]
pub enum Evaluation {
    /// Return a newly allocated primitive.
    Return(Primitive),
    /// Return a contained JavaScript exception.
    Throw(String),
    /// Return a contained backend failure.
    Fail(&'static str),
}

/// Opaque value handle confined to one [`ModelScope`] borrow.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ModelValue<'scope> {
    id: ValueId,
    scope: PhantomData<&'scope ()>,
    affinity: PhantomData<Rc<()>>,
}

/// Opaque generational strong-root handle used by [`ModelBackend`].
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ModelRoot {
    backend: u64,
    slot: usize,
    generation: u64,
}

/// Scoped byte view returned by the deterministic external-buffer model.
#[derive(Debug)]
pub struct ModelBufferView<'view> {
    entry: Ref<'view, ModelValueEntry>,
}

impl AsRef<[u8]> for ModelBufferView<'_> {
    fn as_ref(&self) -> &[u8] {
        match &*self.entry {
            ModelValueEntry::External(bytes) => bytes,
            ModelValueEntry::Primitive(_) => unreachable!("validated buffer changed kind"),
        }
    }
}

/// External-buffer ownership counters from the deterministic model.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ExternalBufferStats {
    /// Number of owners accepted without copying.
    pub accepted: u64,
    /// Number of bytes currently owned by modeled JavaScript buffers.
    pub live_bytes: u64,
    /// Number of owners finalized.
    pub finalized: u64,
    /// Number of payload bytes copied by the external route.
    pub copied_bytes: u64,
}

/// Deterministic source-linked backend used for contract and state-model tests.
///
/// It does not parse JavaScript. Tests enqueue exact evaluation outcomes, making
/// exception and failure ordering reproducible rather than simulating an engine.
#[derive(Debug)]
pub struct ModelBackend {
    id: u64,
    state: RefCell<ModelState>,
    external_fault: Cell<ExternalFault>,
}

/// One scoped entry into [`ModelBackend`].
///
/// Values borrow the scope and cannot escape it:
///
/// ```compile_fail
/// use rustjsi_backend::{BackendBase, BackendScope};
/// use rustjsi_testkit::ModelBackend;
///
/// let mut backend = ModelBackend::new();
/// let value = {
///     let scope = backend.open_scope().unwrap();
///     scope.number(42.0).unwrap()
/// };
/// drop(value);
/// ```
///
/// A scope is also runtime-affine:
///
/// ```compile_fail
/// use rustjsi_testkit::ModelScope;
/// fn require_send<T: Send>() {}
/// require_send::<ModelScope<'static>>();
/// ```
#[derive(Debug)]
pub struct ModelScope<'scope> {
    backend: &'scope ModelBackend,
    locals: RefCell<HashSet<ValueId>>,
    affinity: PhantomData<Rc<()>>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct ValueId {
    backend: u64,
    slot: usize,
    generation: u64,
}

#[derive(Debug, Default)]
struct ModelState {
    values: SlotMap<ModelValueEntry>,
    roots: SlotMap<ValueId>,
    evaluations: VecDeque<Evaluation>,
    stats: ExternalBufferStats,
}

#[derive(Debug)]
enum ModelValueEntry {
    Primitive(Primitive),
    External(Box<[u8]>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ExternalFault {
    None,
    Reject,
    AcceptThenFail,
}

#[derive(Debug)]
struct Slot<T> {
    generation: u64,
    value: Option<T>,
}

#[derive(Debug)]
struct SlotMap<T> {
    slots: Vec<Slot<T>>,
    free: Vec<usize>,
}

impl<T> Default for SlotMap<T> {
    fn default() -> Self {
        Self {
            slots: Vec::new(),
            free: Vec::new(),
        }
    }
}

impl Default for ModelBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl ModelBackend {
    /// Creates an empty deterministic backend.
    ///
    /// # Panics
    ///
    /// Panics only if the process exhausts all nonzero `u64` model identities.
    #[must_use]
    pub fn new() -> Self {
        let id = NEXT_BACKEND_ID
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                current.checked_add(1)
            })
            .expect("deterministic backend ID space exhausted");
        Self {
            id,
            state: RefCell::new(ModelState::default()),
            external_fault: Cell::new(ExternalFault::None),
        }
    }

    /// Enqueues one exact evaluation outcome.
    pub fn push_evaluation(&mut self, evaluation: Evaluation) {
        self.state.get_mut().evaluations.push_back(evaluation);
    }

    /// Makes the next external-buffer transfer fail before ownership changes.
    pub fn reject_next_external_buffer(&mut self) {
        self.external_fault.set(ExternalFault::Reject);
    }

    /// Makes the next external-buffer transfer fail after accepting ownership.
    pub fn fail_next_external_buffer_after_accept(&mut self) {
        self.external_fault.set(ExternalFault::AcceptThenFail);
    }

    /// Returns external-buffer ownership counters.
    #[must_use]
    pub fn external_buffer_stats(&self) -> ExternalBufferStats {
        self.state.borrow().stats
    }
}

impl BackendBase for ModelBackend {
    type Scope<'scope> = ModelScope<'scope>;

    fn manifest(&self) -> BackendManifest {
        BackendManifest::new(
            BACKEND_CONTRACT_VERSION,
            CapabilitySet::only(Capability::StrongRoots).with(Capability::OwnedExternalBuffers),
        )
    }

    fn open_scope(&mut self) -> Result<Self::Scope<'_>, BackendError> {
        Ok(ModelScope {
            backend: self,
            locals: RefCell::new(HashSet::new()),
            affinity: PhantomData,
        })
    }
}

impl Drop for ModelScope<'_> {
    fn drop(&mut self) {
        let locals = std::mem::take(self.locals.get_mut());
        let mut state = self.backend.state.borrow_mut();
        for value in locals {
            if !state.is_rooted(value) {
                state.remove_value(value);
            }
        }
    }
}

impl ModelScope<'_> {
    fn insert(&self, entry: ModelValueEntry) -> ModelValue<'_> {
        let (slot, generation) = self.backend.state.borrow_mut().values.insert(entry);
        let id = ValueId {
            backend: self.backend.id,
            slot,
            generation,
        };
        self.locals.borrow_mut().insert(id);
        self.handle(id)
    }

    fn primitive(&self, value: Primitive) -> ModelValue<'_> {
        self.insert(ModelValueEntry::Primitive(value))
    }

    fn handle(&self, id: ValueId) -> ModelValue<'_> {
        debug_assert_eq!(id.backend, self.backend.id);
        ModelValue {
            id,
            scope: PhantomData,
            affinity: PhantomData,
        }
    }

    fn validate<'value>(
        &'value self,
        value: ModelValue<'value>,
    ) -> Result<ValueKind, BackendError> {
        if value.id.backend != self.backend.id {
            return Err(BackendError::WrongBackend);
        }
        self.backend
            .state
            .borrow()
            .values
            .get(value.id.slot, value.id.generation)
            .map(ModelValueEntry::kind)
            .ok_or(BackendError::StaleHandle)
    }

    fn require_kind<'value>(
        &'value self,
        value: ModelValue<'value>,
        expected: ValueKind,
    ) -> Result<ValueId, BackendError> {
        let actual = self.validate(value)?;
        if actual == expected {
            Ok(value.id)
        } else {
            Err(BackendError::Type { expected, actual })
        }
    }
}

impl BackendScope for ModelScope<'_> {
    type Value<'value>
        = ModelValue<'value>
    where
        Self: 'value;

    fn undefined(&self) -> Result<Self::Value<'_>, BackendError> {
        Ok(self.primitive(Primitive::Undefined))
    }

    fn null(&self) -> Result<Self::Value<'_>, BackendError> {
        Ok(self.primitive(Primitive::Null))
    }

    fn boolean(&self, value: bool) -> Result<Self::Value<'_>, BackendError> {
        Ok(self.primitive(Primitive::Boolean(value)))
    }

    fn number(&self, value: f64) -> Result<Self::Value<'_>, BackendError> {
        Ok(self.primitive(Primitive::Number(value)))
    }

    fn string(&self, value: &str) -> Result<Self::Value<'_>, BackendError> {
        Ok(self.primitive(Primitive::String(value.to_owned())))
    }

    fn evaluate(&self, _source: &str, _source_url: &str) -> Result<Self::Value<'_>, BackendError> {
        let evaluation = self.backend.state.borrow_mut().evaluations.pop_front();
        match evaluation {
            Some(Evaluation::Return(value)) => Ok(self.primitive(value)),
            Some(Evaluation::Throw(message)) => {
                Err(BackendError::Exception(BackendException::new(message)))
            }
            Some(Evaluation::Fail(message)) => Err(BackendError::Failure(message)),
            None => Err(BackendError::Failure(
                "deterministic evaluation was not programmed",
            )),
        }
    }

    fn kind<'value>(&'value self, value: Self::Value<'value>) -> Result<ValueKind, BackendError> {
        self.validate(value)
    }

    fn as_boolean<'value>(&'value self, value: Self::Value<'value>) -> Result<bool, BackendError> {
        let id = self.require_kind(value, ValueKind::Boolean)?;
        match self
            .backend
            .state
            .borrow()
            .values
            .get(id.slot, id.generation)
        {
            Some(ModelValueEntry::Primitive(Primitive::Boolean(value))) => Ok(*value),
            Some(ModelValueEntry::Primitive(_) | ModelValueEntry::External(_)) | None => {
                unreachable!("validated model entry changed kind")
            }
        }
    }

    fn as_number<'value>(&'value self, value: Self::Value<'value>) -> Result<f64, BackendError> {
        let id = self.require_kind(value, ValueKind::Number)?;
        match self
            .backend
            .state
            .borrow()
            .values
            .get(id.slot, id.generation)
        {
            Some(ModelValueEntry::Primitive(Primitive::Number(value))) => Ok(*value),
            Some(ModelValueEntry::Primitive(_) | ModelValueEntry::External(_)) | None => {
                unreachable!("validated model entry changed kind")
            }
        }
    }

    fn to_string<'value>(&'value self, value: Self::Value<'value>) -> Result<String, BackendError> {
        let id = self.require_kind(value, ValueKind::String)?;
        match self
            .backend
            .state
            .borrow()
            .values
            .get(id.slot, id.generation)
        {
            Some(ModelValueEntry::Primitive(Primitive::String(value))) => Ok(value.clone()),
            Some(ModelValueEntry::Primitive(_) | ModelValueEntry::External(_)) | None => {
                unreachable!("validated model entry changed kind")
            }
        }
    }
}

impl RootScope for ModelScope<'_> {
    type Root = ModelRoot;

    fn persist<'value>(
        &'value self,
        value: Self::Value<'value>,
    ) -> Result<Self::Root, BackendError> {
        self.validate(value)?;
        let (slot, generation) = self.backend.state.borrow_mut().roots.insert(value.id);
        Ok(ModelRoot {
            backend: self.backend.id,
            slot,
            generation,
        })
    }

    fn resolve(&self, root: Self::Root) -> Result<Self::Value<'_>, BackendError> {
        if root.backend != self.backend.id {
            return Err(BackendError::WrongBackend);
        }
        let state = self.backend.state.borrow();
        let id = *state
            .roots
            .get(root.slot, root.generation)
            .ok_or(BackendError::StaleHandle)?;
        if state.values.get(id.slot, id.generation).is_none() {
            return Err(BackendError::StaleHandle);
        }
        drop(state);
        self.locals.borrow_mut().insert(id);
        Ok(self.handle(id))
    }

    fn release(&self, root: Self::Root) -> Result<(), BackendError> {
        if root.backend != self.backend.id {
            return Err(BackendError::WrongBackend);
        }
        let mut state = self.backend.state.borrow_mut();
        let value = state
            .roots
            .remove(root.slot, root.generation)
            .ok_or(BackendError::StaleHandle)?;
        let is_local = self.locals.borrow().contains(&value);
        if !is_local && !state.is_rooted(value) {
            state.remove_value(value);
        }
        Ok(())
    }
}

impl OwnedExternalBufferScope for ModelScope<'_> {
    type BufferView<'view>
        = ModelBufferView<'view>
    where
        Self: 'view;

    fn externalize(
        &self,
        owner: Box<[u8]>,
    ) -> Result<Self::Value<'_>, OwnershipTransferError<Box<[u8]>>> {
        let bytes = u64::try_from(owner.len()).unwrap_or(u64::MAX);
        match self.backend.external_fault.replace(ExternalFault::None) {
            ExternalFault::Reject => {
                return Err(OwnershipTransferError::Rejected {
                    error: BackendError::Failure("injected external-buffer rejection"),
                    owner,
                });
            }
            ExternalFault::AcceptThenFail => {
                let mut state = self.backend.state.borrow_mut();
                state.stats.accepted = state.stats.accepted.saturating_add(1);
                state.stats.finalized = state.stats.finalized.saturating_add(1);
                drop(owner);
                return Err(OwnershipTransferError::Accepted {
                    error: BackendError::Failure("injected failure after ownership transfer"),
                });
            }
            ExternalFault::None => {}
        }
        {
            let mut state = self.backend.state.borrow_mut();
            state.stats.accepted = state.stats.accepted.saturating_add(1);
            state.stats.live_bytes = state.stats.live_bytes.saturating_add(bytes);
        }
        Ok(self.insert(ModelValueEntry::External(owner)))
    }

    fn buffer_bytes<'view>(
        &'view self,
        value: Self::Value<'view>,
    ) -> Result<Self::BufferView<'view>, BackendError> {
        self.require_kind(value, ValueKind::Buffer)?;
        let state = self.backend.state.borrow();
        let entry = Ref::filter_map(state, |state| {
            state.values.get(value.id.slot, value.id.generation)
        })
        .map_err(|_| BackendError::StaleHandle)?;
        Ok(ModelBufferView { entry })
    }
}

impl ModelState {
    fn is_rooted(&self, value: ValueId) -> bool {
        self.roots
            .iter()
            .any(|rooted| rooted.is_some_and(|rooted| *rooted == value))
    }

    fn remove_value(&mut self, value: ValueId) {
        if let Some(ModelValueEntry::External(bytes)) =
            self.values.remove(value.slot, value.generation)
        {
            self.stats.live_bytes = self
                .stats
                .live_bytes
                .saturating_sub(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
            self.stats.finalized = self.stats.finalized.saturating_add(1);
        }
    }
}

impl ModelValueEntry {
    const fn kind(&self) -> ValueKind {
        match self {
            Self::Primitive(Primitive::Undefined) => ValueKind::Undefined,
            Self::Primitive(Primitive::Null) => ValueKind::Null,
            Self::Primitive(Primitive::Boolean(_)) => ValueKind::Boolean,
            Self::Primitive(Primitive::Number(_)) => ValueKind::Number,
            Self::Primitive(Primitive::String(_)) => ValueKind::String,
            Self::External(_) => ValueKind::Buffer,
        }
    }
}

impl<T> SlotMap<T> {
    fn insert(&mut self, value: T) -> (usize, u64) {
        if let Some(index) = self.free.pop() {
            let slot = &mut self.slots[index];
            slot.value = Some(value);
            return (index, slot.generation);
        }
        let index = self.slots.len();
        self.slots.push(Slot {
            generation: 1,
            value: Some(value),
        });
        (index, 1)
    }

    fn get(&self, index: usize, generation: u64) -> Option<&T> {
        let slot = self.slots.get(index)?;
        (slot.generation == generation)
            .then_some(slot.value.as_ref())
            .flatten()
    }

    fn remove(&mut self, index: usize, generation: u64) -> Option<T> {
        let slot = self.slots.get_mut(index)?;
        if slot.generation != generation {
            return None;
        }
        let value = slot.value.take()?;
        slot.generation = slot.generation.saturating_add(1);
        if slot.generation != u64::MAX {
            self.free.push(index);
        }
        Some(value)
    }

    fn iter(&self) -> impl Iterator<Item = Option<&T>> {
        self.slots.iter().map(|slot| slot.value.as_ref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustjsi_backend::{BackendBase, BackendScope, OwnedExternalBufferScope, RootScope};

    #[test]
    fn evaluation_outcomes_are_exact_and_ordered() {
        let mut backend = ModelBackend::new();
        backend.push_evaluation(Evaluation::Return(Primitive::Number(42.0)));
        backend.push_evaluation(Evaluation::Throw("boom".to_owned()));

        let scope = backend.open_scope().unwrap();
        let answer = scope.evaluate("ignored", "model.js").unwrap();
        assert!((scope.as_number(answer).unwrap() - 42.0).abs() < f64::EPSILON);
        let error = scope.evaluate("ignored", "model.js").unwrap_err();
        assert!(matches!(error, BackendError::Exception(_)));
    }

    #[test]
    fn roots_are_generational_and_instance_bound() {
        let mut first = ModelBackend::new();
        let root = {
            let scope = first.open_scope().unwrap();
            let value = scope.number(7.0).unwrap();
            scope.persist(value).unwrap()
        };
        {
            let scope = first.open_scope().unwrap();
            let value = scope.resolve(root).unwrap();
            assert!((scope.as_number(value).unwrap() - 7.0).abs() < f64::EPSILON);
            scope.release(root).unwrap();
            assert_eq!(scope.resolve(root), Err(BackendError::StaleHandle));
        }

        let mut second = ModelBackend::new();
        let scope = second.open_scope().unwrap();
        assert_eq!(scope.resolve(root), Err(BackendError::WrongBackend));
    }

    #[test]
    fn external_buffer_preserves_pointer_and_reconciles_ownership() {
        let mut backend = ModelBackend::new();
        let owner = vec![1_u8, 2, 3, 4].into_boxed_slice();
        let pointer = owner.as_ptr();
        {
            let scope = backend.open_scope().unwrap();
            let value = scope.externalize(owner).unwrap();
            let view = scope.buffer_bytes(value).unwrap();
            assert_eq!(view.as_ref().as_ptr(), pointer);
            assert_eq!(view.as_ref(), &[1, 2, 3, 4]);
        }
        assert_eq!(
            backend.external_buffer_stats(),
            ExternalBufferStats {
                accepted: 1,
                live_bytes: 0,
                finalized: 1,
                copied_bytes: 0,
            }
        );
    }

    #[test]
    fn external_failure_returns_the_exact_owner() {
        let mut backend = ModelBackend::new();
        backend.reject_next_external_buffer();
        let owner = vec![9_u8; 32].into_boxed_slice();
        let pointer = owner.as_ptr();
        let scope = backend.open_scope().unwrap();
        let returned = scope.externalize(owner).unwrap_err().into_owner().unwrap();
        assert_eq!(returned.as_ptr(), pointer);
        assert_eq!(returned.len(), 32);
    }

    #[test]
    fn failure_after_transfer_keeps_ownership_in_backend() {
        let mut backend = ModelBackend::new();
        backend.fail_next_external_buffer_after_accept();
        let owner = vec![9_u8; 32].into_boxed_slice();
        let scope = backend.open_scope().unwrap();
        let error = scope.externalize(owner).unwrap_err();
        assert!(matches!(&error, OwnershipTransferError::Accepted { .. }));
        assert!(error.into_owner().is_none());
        drop(scope);
        assert_eq!(
            backend.external_buffer_stats(),
            ExternalBufferStats {
                accepted: 1,
                live_bytes: 0,
                finalized: 1,
                copied_bytes: 0,
            }
        );
    }

    #[test]
    fn rooted_external_buffer_lives_until_release() {
        let mut backend = ModelBackend::new();
        let root = {
            let scope = backend.open_scope().unwrap();
            let value = scope
                .externalize(vec![1_u8; 64].into_boxed_slice())
                .unwrap();
            scope.persist(value).unwrap()
        };
        assert_eq!(backend.external_buffer_stats().live_bytes, 64);
        {
            let scope = backend.open_scope().unwrap();
            scope.release(root).unwrap();
        }
        assert_eq!(backend.external_buffer_stats().live_bytes, 0);
        assert_eq!(backend.external_buffer_stats().finalized, 1);
    }
}
