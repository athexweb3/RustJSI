// SPDX-License-Identifier: MIT OR Apache-2.0

//! Reusable behavioral checks for real and deterministic backends.

use rustjsi_backend::{
    BackendBase, BackendError, BackendScope, BorrowedBufferScope, OwnedExternalBufferScope,
    RootBackend, RootScope, ValueKind,
};

/// Verifies mandatory primitive creation, classification, and strict reads.
///
/// # Errors
///
/// Returns the first backend failure or a conformance mismatch.
pub fn verify_base_values<B>(backend: &mut B) -> Result<(), BackendError>
where
    B: BackendBase,
{
    let scope = backend.open_scope()?;

    let undefined = scope.undefined()?;
    if scope.kind(undefined)? != ValueKind::Undefined {
        return Err(BackendError::Failure("undefined kind mismatch"));
    }

    let null = scope.null()?;
    if scope.kind(null)? != ValueKind::Null {
        return Err(BackendError::Failure("null kind mismatch"));
    }

    let boolean = scope.boolean(true)?;
    if scope.kind(boolean)? != ValueKind::Boolean || !scope.as_boolean(boolean)? {
        return Err(BackendError::Failure("Boolean round-trip mismatch"));
    }

    let number = scope.number(42.5)?;
    if scope.kind(number)? != ValueKind::Number
        || (scope.as_number(number)? - 42.5).abs() > f64::EPSILON
    {
        return Err(BackendError::Failure("number round-trip mismatch"));
    }
    if !matches!(
        scope.as_boolean(number),
        Err(BackendError::Type {
            expected: ValueKind::Boolean,
            actual: ValueKind::Number,
        })
    ) {
        return Err(BackendError::Failure(
            "Boolean read coerced a non-Boolean value",
        ));
    }

    let string = scope.string("RustJSI 🦀")?;
    if scope.kind(string)? != ValueKind::String || scope.to_string(string)? != "RustJSI 🦀" {
        return Err(BackendError::Failure("string round-trip mismatch"));
    }
    if !matches!(
        scope.to_string(boolean),
        Err(BackendError::Type {
            expected: ValueKind::String,
            actual: ValueKind::Boolean,
        })
    ) {
        return Err(BackendError::Failure(
            "string read coerced a non-string value",
        ));
    }

    Ok(())
}

/// Verifies that one backend-level root survives a scope boundary and becomes
/// stale after release.
///
/// # Errors
///
/// Returns the first backend failure or a conformance mismatch.
pub fn verify_strong_root_round_trip<B>(backend: &mut B) -> Result<(), BackendError>
where
    B: RootBackend,
    for<'scope> B::Scope<'scope>: RootScope,
{
    let root = {
        let scope = backend.open_scope()?;
        create_number_root(&scope)?
    };

    {
        let scope = backend.open_scope()?;
        verify_number_root_and_release(&scope, root)?;
    }

    Ok(())
}

/// Creates the standard conformance root in one already-open capable scope.
///
/// This lower-level case is useful for entry-borrowing backends on compilers
/// where a higher-ranked GAT bound would otherwise imply a `'static` backend.
///
/// # Errors
///
/// Returns the first backend failure.
pub fn create_number_root<S>(scope: &S) -> Result<<S::Backend as RootBackend>::Root, BackendError>
where
    S: RootScope,
    S::Backend: RootBackend,
{
    let value = scope.number(73.0)?;
    scope.persist(value)
}

/// Resolves, validates, releases, and stale-checks the standard conformance root.
///
/// # Errors
///
/// Returns the first backend failure or a conformance mismatch.
pub fn verify_number_root_and_release<S>(
    scope: &S,
    root: <S::Backend as RootBackend>::Root,
) -> Result<(), BackendError>
where
    S: RootScope,
    S::Backend: RootBackend,
{
    let value = scope.resolve(root)?;
    if (scope.as_number(value)? - 73.0).abs() > f64::EPSILON {
        return Err(BackendError::Failure("root round-trip mismatch"));
    }
    scope.release(root)?;
    if !matches!(scope.resolve(root), Err(BackendError::StaleHandle)) {
        return Err(BackendError::Failure(
            "released root was not rejected as stale",
        ));
    }
    Ok(())
}

/// Verifies that the external route preserves the exact payload allocation.
///
/// This check requires both ownership transfer and stable borrowed-byte
/// capabilities. Backends may implement the first without implementing the
/// second.
/// For entry-borrowing backends, open a scope and use
/// [`verify_external_buffer_identity_in_scope`] instead.
///
/// # Errors
///
/// Returns the first backend failure, ownership-transfer failure, or a
/// conformance mismatch.
pub fn verify_external_buffer_identity<B>(backend: &mut B) -> Result<(), BackendError>
where
    B: BackendBase,
    for<'scope> B::Scope<'scope>: OwnedExternalBufferScope + BorrowedBufferScope,
{
    verify_external_buffer_identity_in_scope(&backend.open_scope()?)
}

/// Verifies exact allocation identity in an already-open capable scope.
///
/// Borrowed backends can use this check without a higher-ranked scope bound.
/// Both ownership transfer and stable borrowed-byte access are required.
///
/// ```
/// use rustjsi_backend::BackendBase;
/// use rustjsi_testkit::{ModelBackend, verify_external_buffer_identity_in_scope};
///
/// let mut model = ModelBackend::new();
/// model.with_entry(|entry| {
///     let scope = entry.open_scope().unwrap();
///     verify_external_buffer_identity_in_scope(&scope).unwrap();
/// });
/// ```
///
/// # Errors
///
/// Returns the first backend, transfer, or conformance failure.
pub fn verify_external_buffer_identity_in_scope<S>(scope: &S) -> Result<(), BackendError>
where
    S: OwnedExternalBufferScope + BorrowedBufferScope,
{
    let owner = vec![1_u8, 2, 3, 4, 5].into_boxed_slice();
    let pointer = owner.as_ptr();
    let value = scope
        .externalize(owner)
        .map_err(|error| error.error().clone())?;
    let view = scope.buffer_bytes(value)?;
    if view.as_ref().as_ptr() != pointer || view.as_ref() != [1, 2, 3, 4, 5] {
        return Err(BackendError::Failure(
            "external buffer allocation identity mismatch",
        ));
    }
    Ok(())
}

/// Verifies exact-owner acceptance and semantic buffer classification in one
/// already-open scope.
///
/// Pointer identity requires a separate backend-specific observation or the
/// stable borrowed-byte capability; this check does not infer either one.
///
/// # Errors
///
/// Returns the first backend, transfer, or conformance failure.
pub fn verify_owned_external_buffer<S>(scope: &S) -> Result<(), BackendError>
where
    S: OwnedExternalBufferScope,
{
    let value = scope
        .externalize(vec![1_u8, 2, 3, 4, 5].into_boxed_slice())
        .map_err(|error| error.error().clone())?;
    if scope.kind(value)? != ValueKind::Buffer {
        return Err(BackendError::Failure(
            "externalized value was not classified as a buffer",
        ));
    }
    Ok(())
}
