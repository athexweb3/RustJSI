// SPDX-License-Identifier: MIT OR Apache-2.0

//! Capability-oriented engine backend contract for `RustJSI`.
//!
//! This crate is the low-level seam between safe `RustJSI` runtime policy and an
//! engine implementation. Implementations own the unsafe engine mechanics and
//! expose only contained, validated operations through this contract.
//!
//! The contract is experimental and source-linked. It is not a stable ABI.

#![forbid(unsafe_code)]

use std::error::Error;
use std::fmt;

/// The current source-level backend contract version.
pub const BACKEND_CONTRACT_VERSION: ContractVersion = ContractVersion::new(1, 0);

/// A version of the source-level backend contract.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ContractVersion {
    major: u16,
    minor: u16,
}

impl ContractVersion {
    /// Creates a contract version.
    #[must_use]
    pub const fn new(major: u16, minor: u16) -> Self {
        Self { major, minor }
    }

    /// Returns the breaking-change component.
    #[must_use]
    pub const fn major(self) -> u16 {
        self.major
    }

    /// Returns the additive-change component.
    #[must_use]
    pub const fn minor(self) -> u16 {
        self.minor
    }

    /// Reports whether this implementation can satisfy `required`.
    #[must_use]
    pub const fn supports(self, required: Self) -> bool {
        self.major == required.major && self.minor >= required.minor
    }
}

/// A backend behavior that is not part of the mandatory base contract.
///
/// Discriminants are stable capability IDs. Existing IDs must never be
/// renumbered or assigned a new meaning.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(u8)]
pub enum Capability {
    /// Strong persistent roots with explicit release.
    StrongRoots = 0,
    /// Opaque native state with a finalization notification.
    NativeState = 1,
    /// Rust-owned bytes exposed without a payload copy.
    OwnedExternalBuffers = 2,
    /// A deterministic or engine-provided forced-GC test hook.
    ForcedGarbageCollection = 3,
    /// Stable, read-only byte borrows from JavaScript buffer values.
    BorrowedBufferBytes = 4,
}

impl Capability {
    const fn mask(self) -> u64 {
        1_u64 << (self as u8)
    }

    /// Returns the diagnostic name of this capability.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::StrongRoots => "strong-roots",
            Self::NativeState => "native-state",
            Self::OwnedExternalBuffers => "owned-external-buffers",
            Self::ForcedGarbageCollection => "forced-garbage-collection",
            Self::BorrowedBufferBytes => "borrowed-buffer-bytes",
        }
    }
}

/// A compact set of backend capabilities.
#[derive(Clone, Copy, Default, Eq, Hash, PartialEq)]
pub struct CapabilitySet(u64);

impl CapabilitySet {
    /// Creates an empty set.
    #[must_use]
    pub const fn empty() -> Self {
        Self(0)
    }

    /// Creates a set containing one capability.
    #[must_use]
    pub const fn only(capability: Capability) -> Self {
        Self(capability.mask())
    }

    /// Returns a set with `capability` included.
    #[must_use]
    pub const fn with(self, capability: Capability) -> Self {
        Self(self.0 | capability.mask())
    }

    /// Reports whether the set contains `capability`.
    #[must_use]
    pub const fn contains(self, capability: Capability) -> bool {
        self.0 & capability.mask() != 0
    }

    /// Reports whether every capability in `required` is present.
    #[must_use]
    pub const fn contains_all(self, required: Self) -> bool {
        self.0 & required.0 == required.0
    }

    /// Returns capabilities present in `self` but absent from `other`.
    #[must_use]
    pub const fn difference(self, other: Self) -> Self {
        Self(self.0 & !other.0)
    }

    /// Returns whether the set is empty.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Iterates over known capabilities in stable ID order.
    pub fn iter(self) -> impl Iterator<Item = Capability> {
        const KNOWN: [Capability; 5] = [
            Capability::StrongRoots,
            Capability::NativeState,
            Capability::OwnedExternalBuffers,
            Capability::ForcedGarbageCollection,
            Capability::BorrowedBufferBytes,
        ];
        KNOWN.into_iter().filter(move |item| self.contains(*item))
    }
}

impl fmt::Debug for CapabilitySet {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_set().entries(self.iter()).finish()
    }
}

/// Immutable identity and capability information for one backend instance.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BackendManifest {
    contract: ContractVersion,
    capabilities: CapabilitySet,
}

impl BackendManifest {
    /// Creates a manifest.
    #[must_use]
    pub const fn new(contract: ContractVersion, capabilities: CapabilitySet) -> Self {
        Self {
            contract,
            capabilities,
        }
    }

    /// Returns the implemented contract version.
    #[must_use]
    pub const fn contract(self) -> ContractVersion {
        self.contract
    }

    /// Returns the advertised capability set.
    #[must_use]
    pub const fn capabilities(self) -> CapabilitySet {
        self.capabilities
    }

    /// Verifies a contract version and required capabilities.
    ///
    /// # Errors
    ///
    /// Returns the precise compatibility failure without selecting a fallback.
    pub fn require(
        self,
        contract: ContractVersion,
        capabilities: CapabilitySet,
    ) -> Result<(), CompatibilityError> {
        if !self.contract.supports(contract) {
            return Err(CompatibilityError::ContractVersion {
                required: contract,
                provided: self.contract,
            });
        }
        let missing = capabilities.difference(self.capabilities);
        if missing.is_empty() {
            Ok(())
        } else {
            Err(CompatibilityError::MissingCapabilities(missing))
        }
    }
}

/// A backend manifest compatibility failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompatibilityError {
    /// The backend contract major differs or its minor is too old.
    ContractVersion {
        /// Contract requested by the caller.
        required: ContractVersion,
        /// Contract implemented by the backend.
        provided: ContractVersion,
    },
    /// One or more required capabilities are absent.
    MissingCapabilities(CapabilitySet),
}

impl fmt::Display for CompatibilityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ContractVersion { required, provided } => write!(
                formatter,
                "backend contract {}.{} cannot satisfy {}.{}",
                provided.major, provided.minor, required.major, required.minor
            ),
            Self::MissingCapabilities(missing) => {
                formatter.write_str("backend is missing capabilities: ")?;
                for (index, capability) in missing.iter().enumerate() {
                    if index != 0 {
                        formatter.write_str(", ")?;
                    }
                    formatter.write_str(capability.name())?;
                }
                Ok(())
            }
        }
    }
}

impl Error for CompatibilityError {}

/// The engine-independent kind of a scoped JavaScript value.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ValueKind {
    /// JavaScript `undefined`.
    Undefined,
    /// JavaScript `null`.
    Null,
    /// A Boolean.
    Boolean,
    /// A number.
    Number,
    /// A string.
    String,
    /// A symbol primitive.
    Symbol,
    /// A `BigInt` primitive.
    BigInt,
    /// An object that is not refined further by the base contract.
    Object,
    /// A callable object.
    Function,
    /// A binary-buffer object.
    Buffer,
}

/// An exception copied out of an engine call.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BackendException {
    message: String,
}

impl BackendException {
    /// Creates owned exception metadata.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    /// Returns the captured message.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for BackendException {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for BackendException {}

/// A low-level backend operation failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BackendError {
    /// JavaScript raised an exception.
    Exception(BackendException),
    /// A raw handle came from another backend instance.
    WrongBackend,
    /// A raw handle no longer identifies a live entry.
    StaleHandle,
    /// A value has the wrong JavaScript kind.
    Type {
        /// Expected kind.
        expected: ValueKind,
        /// Observed kind.
        actual: ValueKind,
    },
    /// The requested optional behavior is not supported.
    Unsupported(Capability),
    /// The engine or deterministic model reported a contained failure.
    Failure(&'static str),
}

/// Failure from an operation that may transfer ownership before it can fail.
#[derive(Debug)]
pub enum OwnershipTransferError<T> {
    /// The backend rejected the operation without taking ownership.
    Rejected {
        /// Contained backend failure.
        error: BackendError,
        /// Original owner returned unchanged.
        owner: T,
    },
    /// The backend accepted ownership before a later operation failed.
    ///
    /// The backend remains responsible for eventually releasing the owner.
    Accepted {
        /// Contained backend failure.
        error: BackendError,
    },
}

impl<T> OwnershipTransferError<T> {
    /// Returns the contained backend failure.
    #[must_use]
    pub const fn error(&self) -> &BackendError {
        match self {
            Self::Rejected { error, .. } | Self::Accepted { error } => error,
        }
    }

    /// Returns the original owner only when transfer never occurred.
    #[must_use]
    pub fn into_owner(self) -> Option<T> {
        match self {
            Self::Rejected { owner, .. } => Some(owner),
            Self::Accepted { .. } => None,
        }
    }
}

impl fmt::Display for BackendError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Exception(error) => write!(formatter, "JavaScript exception: {error}"),
            Self::WrongBackend => formatter.write_str("handle belongs to another backend"),
            Self::StaleHandle => formatter.write_str("handle is stale"),
            Self::Type { expected, actual } => {
                write!(formatter, "expected {expected:?}, found {actual:?}")
            }
            Self::Unsupported(capability) => {
                write!(formatter, "unsupported capability: {}", capability.name())
            }
            Self::Failure(message) => formatter.write_str(message),
        }
    }
}

impl Error for BackendError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Exception(error) => Some(error),
            Self::WrongBackend
            | Self::StaleHandle
            | Self::Type { .. }
            | Self::Unsupported(_)
            | Self::Failure(_) => None,
        }
    }
}

/// Mandatory operations available while an engine scope is active.
///
/// # Implementation contract
///
/// Every returned raw value must belong to this scope's backend instance and
/// remain valid until the scope ends. Implementations must contain
/// engine exceptions and foreign unwinding, reject foreign/stale handles, and
/// obey the engine's thread and scope rules. A method may execute JavaScript,
/// allocate, trigger GC, or re-enter unless its documentation says otherwise.
pub trait BackendScope {
    /// Backend instance that created this scope.
    type Backend: BackendBase;

    /// Backend-private value handle confined to this scope borrow.
    type Value<'value>: Copy + fmt::Debug + Eq
    where
        Self: 'value;

    /// Creates JavaScript `undefined`.
    ///
    /// # Errors
    ///
    /// Returns a contained allocation or engine failure.
    fn undefined(&self) -> Result<Self::Value<'_>, BackendError>;

    /// Creates JavaScript `null`.
    ///
    /// # Errors
    ///
    /// Returns a contained allocation or engine failure.
    fn null(&self) -> Result<Self::Value<'_>, BackendError>;

    /// Creates a JavaScript Boolean.
    ///
    /// # Errors
    ///
    /// Returns a contained allocation or engine failure.
    fn boolean(&self, value: bool) -> Result<Self::Value<'_>, BackendError>;

    /// Creates a JavaScript number.
    ///
    /// # Errors
    ///
    /// Returns a contained allocation or engine failure.
    fn number(&self, value: f64) -> Result<Self::Value<'_>, BackendError>;

    /// Creates a JavaScript string.
    ///
    /// # Errors
    ///
    /// Returns a contained encoding, allocation, or engine failure.
    fn string(&self, value: &str) -> Result<Self::Value<'_>, BackendError>;

    /// Evaluates a source unit.
    ///
    /// # Errors
    ///
    /// Returns a captured JavaScript exception or contained engine failure.
    fn evaluate(&self, source: &str, source_url: &str) -> Result<Self::Value<'_>, BackendError>;

    /// Returns a value's semantic kind without coercion.
    ///
    /// # Errors
    ///
    /// Rejects foreign or stale values and contained engine failures.
    fn kind<'value>(&'value self, value: Self::Value<'value>) -> Result<ValueKind, BackendError>;

    /// Reads a Boolean without coercion.
    ///
    /// # Errors
    ///
    /// Rejects foreign, stale, or non-Boolean values.
    fn as_boolean<'value>(&'value self, value: Self::Value<'value>) -> Result<bool, BackendError>;

    /// Reads a number without coercion.
    ///
    /// # Errors
    ///
    /// Rejects foreign, stale, or non-number values.
    fn as_number<'value>(&'value self, value: Self::Value<'value>) -> Result<f64, BackendError>;

    /// Copies a string to Rust UTF-8 without JavaScript coercion.
    ///
    /// # Errors
    ///
    /// Rejects foreign, stale, or non-string values and conversion failures.
    fn to_string<'value>(&'value self, value: Self::Value<'value>) -> Result<String, BackendError>;
}

/// Mandatory source-linked backend entry contract.
///
/// # Implementation contract
///
/// Implementors create scopes only under legal engine entry, on the
/// correct thread/isolate, and must not let a scope outlive its engine runtime.
/// The manifest must describe actual behavior; advertising an unsupported or
/// semantically different capability is a contract violation.
pub trait BackendBase {
    /// Scope type borrowing this backend for one legal engine entry.
    type Scope<'scope>: BackendScope<Backend = Self>
    where
        Self: 'scope;

    /// Returns immutable compatibility information.
    fn manifest(&self) -> BackendManifest;

    /// Opens one legal engine scope.
    ///
    /// # Errors
    ///
    /// Returns a contained backend failure when entry cannot be established.
    fn open_scope(&mut self) -> Result<Self::Scope<'_>, BackendError>;
}

/// Backend-level strong-root identity for engines that support persistent roots.
///
/// A root crosses individual engine entries, so its type belongs to the backend
/// instance rather than to any one scope type.
pub trait RootBackend: BackendBase {
    /// Backend-private, instance-bound persistent-root handle.
    type Root: Copy + fmt::Debug + Eq;
}

/// Strong-root operations available on a capable backend scope.
///
/// # Implementation contract
///
/// Root handles are instance-bound and generational. `persist` keeps the
/// value alive after the current scope, `resolve` must reject stale and foreign
/// handles, and `release` must be idempotently safe at the implementation's raw
/// boundary. Engine access and destruction must occur in the legal domain.
pub trait RootScope: BackendScope
where
    Self::Backend: RootBackend,
{
    /// Creates a strong root for a scoped value.
    ///
    /// # Errors
    ///
    /// Rejects foreign/stale values and contained root failures.
    fn persist<'value>(
        &'value self,
        value: Self::Value<'value>,
    ) -> Result<<Self::Backend as RootBackend>::Root, BackendError>;

    /// Resolves a root into the current scope.
    ///
    /// # Errors
    ///
    /// Rejects foreign, released, or stale roots.
    fn resolve(
        &self,
        root: <Self::Backend as RootBackend>::Root,
    ) -> Result<Self::Value<'_>, BackendError>;

    /// Releases a root.
    ///
    /// # Errors
    ///
    /// Rejects foreign, already released, or stale roots.
    fn release(&self, root: <Self::Backend as RootBackend>::Root) -> Result<(), BackendError>;
}

/// Rust-owned external-buffer operations available on a capable scope.
///
/// # Implementation contract
///
/// The backend transfers the exact allocation into an engine buffer without
/// copying or reports whether transfer happened before failure. It must preserve
/// Rust aliasing and allocator provenance, and the allocation must be dropped
/// exactly once even when finalization happens late or on another engine thread.
pub trait OwnedExternalBufferScope: BackendScope {
    /// Transfers exact-length Rust-owned bytes into a JavaScript buffer.
    ///
    /// Hidden copying is not permitted by this operation.
    ///
    /// # Errors
    ///
    /// Distinguishes rejection before transfer from failure after the backend
    /// accepted ownership.
    fn externalize(
        &self,
        owner: Box<[u8]>,
    ) -> Result<Self::Value<'_>, OwnershipTransferError<Box<[u8]>>>;
}

/// Stable read-only buffer borrows available on a capable scope.
///
/// # Implementation contract
///
/// The bytes must remain valid and immutable for the entire returned view
/// lifetime, including across any engine work the safe API permits while the
/// view exists. A backend whose engine exposes only a temporary pointer must not
/// implement this trait. Externalizing Rust-owned bytes does not by itself prove
/// this separate capability.
pub trait BorrowedBufferScope: BackendScope {
    /// A borrow of buffer bytes confined to this scope borrow.
    type BufferView<'view>: AsRef<[u8]>
    where
        Self: 'view;

    /// Borrows buffer bytes for no longer than this scope borrow.
    ///
    /// # Errors
    ///
    /// Rejects foreign, stale, or non-buffer values.
    fn buffer_bytes<'view>(
        &'view self,
        value: Self::Value<'view>,
    ) -> Result<Self::BufferView<'view>, BackendError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_negotiation_is_explicit() {
        let provided =
            CapabilitySet::only(Capability::StrongRoots).with(Capability::OwnedExternalBuffers);
        let manifest = BackendManifest::new(BACKEND_CONTRACT_VERSION, provided);

        manifest
            .require(
                BACKEND_CONTRACT_VERSION,
                CapabilitySet::only(Capability::StrongRoots),
            )
            .unwrap();

        let error = manifest
            .require(
                BACKEND_CONTRACT_VERSION,
                CapabilitySet::only(Capability::NativeState),
            )
            .unwrap_err();
        assert_eq!(
            error,
            CompatibilityError::MissingCapabilities(CapabilitySet::only(Capability::NativeState))
        );
    }

    #[test]
    fn contract_version_requires_equal_major_and_sufficient_minor() {
        assert!(ContractVersion::new(1, 4).supports(ContractVersion::new(1, 3)));
        assert!(!ContractVersion::new(1, 2).supports(ContractVersion::new(1, 3)));
        assert!(!ContractVersion::new(2, 0).supports(ContractVersion::new(1, 3)));
    }
}
