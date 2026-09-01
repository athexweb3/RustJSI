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
        let value = scope.number(73.0)?;
        scope.persist(value)?
    };

    {
        let scope = backend.open_scope()?;
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
    }

    Ok(())
}

/// Verifies that the external route preserves the exact payload allocation.
///
/// This check requires both ownership transfer and stable borrowed-byte
/// capabilities. Backends may implement the first without implementing the
/// second.
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
    let owner = vec![1_u8, 2, 3, 4, 5].into_boxed_slice();
    let pointer = owner.as_ptr();
    let scope = backend.open_scope()?;
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
