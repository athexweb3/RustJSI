// SPDX-License-Identifier: MIT OR Apache-2.0

//! Conformance checks for deterministic host-owned drain registration.

use std::num::NonZeroUsize;

use rustjsi_host::{
    AttachmentWorkOwner, AttachmentWorkResolution, DrainPoster, DrainRegistration,
    DrainTaskResolution, RuntimeIdentity, ScheduledWorkAcquire, ScheduledWorkMailbox,
};
use rustjsi_runtime::{DrainAfter, MailboxEnqueue};
use rustjsi_testkit::{DrainPostAcquire, DrainPostQueue, ModelHost};

#[test]
fn queued_old_attachment_task_cannot_target_its_replacement() {
    let mut identity = RuntimeIdentity::allocate().unwrap();
    let old = identity.next_attachment().unwrap();
    let replacement = identity.next_attachment().unwrap();
    let posts = DrainPostQueue::new(NonZeroUsize::new(1).unwrap());
    let mut registration = DrainRegistration::new(old);

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

#[test]
fn replacement_keeps_retired_posts_and_payloads_out_of_the_new_attachment() {
    let mut identity = RuntimeIdentity::allocate().unwrap();
    let old = identity.next_attachment().unwrap();
    let replacement = identity.next_attachment().unwrap();
    let posts = DrainPostQueue::new(NonZeroUsize::new(2).unwrap());
    let old_mailbox = ScheduledWorkMailbox::new(old, NonZeroUsize::new(1).unwrap());
    let mut registration = DrainRegistration::new(old);

    let _ = old_mailbox.enqueue_and_post(&posts, 7_u8).unwrap();
    registration.begin_close();
    let _ = old_mailbox.begin_close();
    let terminal = old_mailbox.try_begin_terminal_drain().unwrap();
    assert_eq!(terminal.pop().unwrap().into_payload(), 7);
    terminal.finish().unwrap();
    assert!(old_mailbox.is_terminally_closed());

    registration.replace(replacement).unwrap();
    let replacement_mailbox = ScheduledWorkMailbox::new(replacement, NonZeroUsize::new(1).unwrap());
    let _ = replacement_mailbox.enqueue_and_post(&posts, 41_u8).unwrap();

    let DrainPostAcquire::Acquired(post_drain) = posts.acquire() else {
        panic!("both accepted attachment posts must be observable");
    };
    assert_eq!(post_drain.pop(), Some(old));
    assert_eq!(registration.resolve(old), DrainTaskResolution::Retired);
    assert_eq!(post_drain.pop(), Some(replacement));
    assert_eq!(
        registration.resolve(replacement),
        DrainTaskResolution::Current
    );
    assert_eq!(post_drain.finish(), DrainAfter::Idle);

    let mut host = ModelHost::for_attachment(replacement);
    let ScheduledWorkAcquire::Acquired(work_drain) = replacement_mailbox.acquire() else {
        panic!("only the replacement mailbox may retain current work");
    };
    assert_eq!(
        work_drain
            .dispatch_next(&mut host, |_, payload| payload + 1)
            .unwrap(),
        Some(42)
    );
    assert_eq!(work_drain.finish(), DrainAfter::Idle);
}

#[test]
fn attachment_work_owner_retires_old_posts_and_lends_only_current_mailbox() {
    let mut identity = RuntimeIdentity::allocate().unwrap();
    let old = identity.next_attachment().unwrap();
    let replacement = identity.next_attachment().unwrap();
    let posts = DrainPostQueue::new(NonZeroUsize::new(2).unwrap());
    let mut owner = AttachmentWorkOwner::new(old, NonZeroUsize::new(1).unwrap());
    let old_sender = owner.sender();

    assert!(matches!(
        old_sender.enqueue_and_post(&posts, 7_u8),
        Ok(MailboxEnqueue::Scheduled)
    ));
    let _ = owner.begin_close();
    assert!(matches!(
        owner.resolve_drain_task(old),
        AttachmentWorkResolution::Closing
    ));

    let terminal = owner.try_begin_terminal_drain().unwrap();
    assert_eq!(terminal.pop().unwrap().into_payload(), 7);
    terminal.finish().unwrap();
    owner
        .replace(replacement, NonZeroUsize::new(1).unwrap())
        .unwrap();

    let replacement_sender = owner.sender();
    assert!(matches!(
        replacement_sender.enqueue_and_post(&posts, 41_u8),
        Ok(MailboxEnqueue::Scheduled)
    ));

    let DrainPostAcquire::Acquired(post_drain) = posts.acquire() else {
        panic!("accepted attachment posts must remain observable");
    };
    assert_eq!(post_drain.pop(), Some(old));
    assert!(matches!(
        owner.resolve_drain_task(old),
        AttachmentWorkResolution::Retired
    ));
    assert_eq!(post_drain.pop(), Some(replacement));

    let AttachmentWorkResolution::Current(mailbox) = owner.resolve_drain_task(replacement) else {
        panic!("replacement task must lend only the current mailbox");
    };
    let mut host = ModelHost::for_attachment(replacement);
    let ScheduledWorkAcquire::Acquired(work_drain) = mailbox.acquire() else {
        panic!("current mailbox must retain replacement work");
    };
    assert_eq!(
        work_drain
            .dispatch_next(&mut host, |_, payload| payload + 1)
            .unwrap(),
        Some(42)
    );
    assert_eq!(work_drain.finish(), DrainAfter::Idle);
    assert_eq!(post_drain.finish(), DrainAfter::Idle);
}
