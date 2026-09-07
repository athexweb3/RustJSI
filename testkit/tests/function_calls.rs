// SPDX-License-Identifier: MIT OR Apache-2.0

//! Deterministic function-call admission and outcome ordering.

use rustjsi_backend::{BackendBase, BackendError, BackendScope, CallReceiver};
use rustjsi_testkit::{Evaluation, Invocation, ModelBackend, Primitive};

#[test]
fn rejected_values_do_not_consume_invocations() {
    let mut model = ModelBackend::new();
    model.push_evaluation(Evaluation::ReturnFunction);
    model.push_invocation(Invocation::Return(Primitive::Number(42.0)));
    model.push_invocation(Invocation::Throw("expected".into()));
    model.with_entry(|entry| {
        let scope = entry.open_scope().unwrap();
        let function = scope.evaluate("", "").unwrap();
        let number = scope.number(1.0).unwrap();
        assert!(matches!(
            scope.call(number, CallReceiver::Global, &[]),
            Err(BackendError::Type { .. })
        ));
        assert!(matches!(
            scope.call(function, CallReceiver::Object(number), &[]),
            Err(BackendError::Type { .. })
        ));
        let mut foreign = ModelBackend::new();
        let foreign_scope = foreign.open_scope().unwrap();
        let foreign_value = foreign_scope.number(2.0).unwrap();
        assert_eq!(
            scope.call(function, CallReceiver::Global, &[foreign_value]),
            Err(BackendError::WrongBackend)
        );
        let result = scope
            .call(function, CallReceiver::Global, &[number])
            .unwrap();
        assert_eq!(scope.as_number(result), Ok(42.0));
        assert!(matches!(
            scope.call(function, CallReceiver::Global, &[]),
            Err(BackendError::Exception(_))
        ));
        assert!(matches!(
            scope.call(function, CallReceiver::Global, &[]),
            Err(BackendError::Failure(_))
        ));
    });
}
