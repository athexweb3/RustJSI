// SPDX-License-Identifier: MIT OR Apache-2.0

//! Conformance checks for deterministic host-owned drain registration.

use std::num::NonZeroUsize;

use rustjsi_host::{DrainPoster, RuntimeIdentity};
use rustjsi_runtime::DrainAfter;
use rustjsi_testkit::{
    DrainPostAcquire, DrainPostQueue, DrainRegistrationModel, DrainTaskResolution,
};

#[test]
fn queued_old_attachment_task_cannot_target_its_replacement() {
    let mut identity = RuntimeIdentity::allocate().unwrap();
    let old = identity.next_attachment().unwrap();
    let replacement = identity.next_attachment().unwrap();
    let posts = DrainPostQueue::new(NonZeroUsize::new(1).unwrap());
    let mut registration = DrainRegistrationModel::new(old);

    posts.post_drain(old).unwrap();
    registration.begin_close();
    registration.replace(replacement).unwrap();

    let DrainPostAcquire::Acquired(drain) = posts.acquire() else {
        panic!("accepted old task must remain observable to the host owner");
    };
    let attachment = drain.pop().expect("drain must retain the old task");
    assert_eq!(
        registration.resolve(attachment),
        DrainTaskResolution::Retired
    );
    assert_eq!(drain.finish(), DrainAfter::Idle);
}
