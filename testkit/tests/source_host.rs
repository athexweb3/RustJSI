// SPDX-License-Identifier: MIT OR Apache-2.0

//! Source-linked Host contract checks over the deterministic backend.

use rustjsi_backend::BackendError;
use rustjsi_host::{GateError, Host, HostState};
use rustjsi_testkit::{ModelHost, verify_base_values};
use std::cell::Cell;

fn verify_host<H: Host>(host: &mut H) -> Result<Result<(), BackendError>, H::Error> {
    let attachment = host.attachment_id();
    assert_eq!(host.state(), HostState::Active);
    let result = host.with_backend(|backend| verify_base_values(backend));
    assert_eq!(host.attachment_id(), attachment);
    result
}

#[test]
fn owning_and_borrowed_model_hosts_share_the_contract() {
    let mut host = ModelHost::new().unwrap();
    let attachment = host.attachment_id();
    verify_host(&mut host).unwrap().unwrap();
    verify_host(&mut &mut host).unwrap().unwrap();
    assert_eq!(host.attachment_id(), attachment);
}

#[test]
fn draining_rejects_entry_without_running_the_operation() {
    let mut host = ModelHost::new().unwrap();
    let called = Cell::new(false);
    host.request_drain();

    assert_eq!(
        host.with_backend(|_| called.set(true)),
        Err(GateError::NotActive(HostState::Draining))
    );
    assert!(!called.get());
    host.finish_drain().unwrap();
    host.mark_destroyed().unwrap();
    assert_eq!(host.state(), HostState::Destroyed);
}
