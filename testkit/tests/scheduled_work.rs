// SPDX-License-Identifier: MIT OR Apache-2.0

//! Conformance tests for attachment-bound host work delivery.

use rustjsi_host::{Host, RuntimeIdentity, ScheduledWork, WorkDispatchError};
use rustjsi_testkit::ModelHost;

#[test]
fn current_attachment_consumes_work_inside_one_host_entry() {
    let mut host = ModelHost::new().unwrap();
    let work = ScheduledWork::new(host.attachment_id(), 41_u32);

    let value = work.dispatch(&mut host, |_, payload| payload + 1).unwrap();
    assert_eq!(value, 42);
}

#[test]
fn older_epoch_is_rejected_before_host_entry_and_keeps_payload() {
    let mut identity = RuntimeIdentity::allocate().unwrap();
    let stale = identity.next_attachment().unwrap();
    let current = identity.next_attachment().unwrap();
    let mut host = ModelHost::for_attachment(current);
    let work = ScheduledWork::new(stale, String::from("stale"));
    let mut invoked = false;

    let error = work
        .dispatch(&mut host, |_, _| {
            invoked = true;
        })
        .expect_err("an earlier attachment epoch must not enter the host");

    match error {
        WorkDispatchError::AttachmentMismatch {
            active_attachment,
            work,
        } => {
            assert_eq!(active_attachment, current);
            assert_eq!(work.attachment_id(), stale);
            assert_eq!(work.into_payload(), "stale");
        }
        other => panic!("expected attachment mismatch, got {other:?}"),
    }
    assert!(!invoked);
}

#[test]
fn foreign_runtime_is_rejected_before_host_entry_and_keeps_payload() {
    let mut host = ModelHost::new().unwrap();
    let mut foreign_identity = RuntimeIdentity::allocate().unwrap();
    let foreign = foreign_identity.next_attachment().unwrap();
    let work = ScheduledWork::new(foreign, 7_u32);

    let error = work
        .dispatch(&mut host, |_, _| panic!("foreign work must not enter"))
        .expect_err("foreign runtime work must be rejected");

    let work = error
        .into_work()
        .expect("identity rejection must retain its work record");
    assert_eq!(work.attachment_id(), foreign);
    assert_eq!(work.into_payload(), 7);
}

#[test]
fn draining_host_rejects_matching_work_without_consuming_payload() {
    let mut host = ModelHost::new().unwrap();
    let attachment = host.attachment_id();
    host.request_drain();
    let work = ScheduledWork::new(attachment, vec![1_u8, 2, 3]);

    let error = work
        .dispatch(&mut host, |_, _| {
            panic!("draining host must not invoke work")
        })
        .expect_err("draining host must reject work");

    match error {
        WorkDispatchError::Entry { error, work } => {
            assert!(error.to_string().contains("not active"));
            assert_eq!(work.attachment_id(), attachment);
            assert_eq!(work.into_payload(), vec![1, 2, 3]);
        }
        other => panic!("expected host entry rejection, got {other:?}"),
    }
}
