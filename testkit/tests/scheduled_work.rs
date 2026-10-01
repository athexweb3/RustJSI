// SPDX-License-Identifier: MIT OR Apache-2.0

//! Conformance tests for attachment-bound host work delivery.

use std::cell::Cell;
use std::convert::Infallible;
use std::error::Error;
use std::fmt;
use std::num::NonZeroUsize;

use rustjsi_host::{
    DrainPoster, Host, RuntimeIdentity, ScheduledWork, ScheduledWorkAcquire, ScheduledWorkMailbox,
    ScheduledWorkPostError, WorkDispatchError,
};
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

#[test]
fn mailbox_drain_dispatches_one_record_through_the_host() {
    let mut host = ModelHost::new().unwrap();
    let mailbox = ScheduledWorkMailbox::new(NonZeroUsize::new(1).unwrap());
    let _ = mailbox.enqueue(host.attachment_id(), 41_u32).unwrap();

    let ScheduledWorkAcquire::Acquired(drain) = mailbox.acquire() else {
        panic!("queued work must acquire a normal drain");
    };
    let value = drain
        .dispatch_next(&mut host, |_, payload| payload + 1)
        .unwrap();
    assert_eq!(value, Some(42));

    let called = Cell::new(false);
    assert_eq!(
        drain
            .dispatch_next(&mut host, |_, _| {
                called.set(true);
            })
            .unwrap(),
        None
    );
    assert!(!called.get());
    assert_eq!(drain.finish(), rustjsi_runtime::DrainAfter::Idle);
}

#[test]
fn mailbox_drain_returns_stale_work_without_host_entry() {
    let mut identity = RuntimeIdentity::allocate().unwrap();
    let stale = identity.next_attachment().unwrap();
    let current = identity.next_attachment().unwrap();
    let mailbox = ScheduledWorkMailbox::new(NonZeroUsize::new(1).unwrap());
    let _ = mailbox.enqueue(stale, String::from("stale")).unwrap();
    let mut host = ModelHost::for_attachment(current);

    let ScheduledWorkAcquire::Acquired(drain) = mailbox.acquire() else {
        panic!("queued stale work must acquire a normal drain");
    };
    let error = drain
        .dispatch_next(&mut host, |_, _| panic!("stale work must not enter"))
        .expect_err("stale attachment must reject before host entry");
    let work = error
        .into_work()
        .expect("attachment rejection must retain queued work");
    assert_eq!(work.attachment_id(), stale);
    assert_eq!(work.into_payload(), "stale");
    assert_eq!(drain.finish(), rustjsi_runtime::DrainAfter::Idle);
}

#[test]
fn terminal_mailbox_drain_retains_stale_attachment_for_caller_policy() {
    let mut identity = RuntimeIdentity::allocate().unwrap();
    let stale = identity.next_attachment().unwrap();
    let current = identity.next_attachment().unwrap();
    let mailbox = ScheduledWorkMailbox::new(NonZeroUsize::new(1).unwrap());
    let _ = mailbox.enqueue(stale, String::from("stale")).unwrap();
    let _ = mailbox.begin_close();

    let terminal = mailbox.try_begin_terminal_drain().unwrap();
    let work = terminal
        .pop()
        .expect("residual work must transfer to caller");
    terminal.finish().unwrap();

    let mut host = ModelHost::for_attachment(current);
    let error = work
        .dispatch(&mut host, |_, _| {
            panic!("stale terminal work must not enter")
        })
        .expect_err("caller must still receive normal attachment rejection");
    let work = error.into_work().expect("rejection retains terminal work");
    assert_eq!(work.attachment_id(), stale);
    assert_eq!(work.into_payload(), "stale");
}

#[derive(Default)]
struct RecordingPoster {
    posts: Cell<usize>,
}

impl DrainPoster for RecordingPoster {
    type Error = Infallible;

    fn post_drain(&self, _attachment: rustjsi_host::AttachmentId) -> Result<(), Self::Error> {
        self.posts.set(self.posts.get() + 1);
        Ok(())
    }
}

#[test]
fn mailbox_posts_once_for_a_coalesced_drain() {
    let host = ModelHost::new().unwrap();
    let mailbox = ScheduledWorkMailbox::new(NonZeroUsize::new(2).unwrap());
    let poster = RecordingPoster::default();

    let _ = mailbox
        .enqueue_and_post(&poster, host.attachment_id(), 1_u32)
        .unwrap();
    let _ = mailbox
        .enqueue_and_post(&poster, host.attachment_id(), 2_u32)
        .unwrap();

    assert_eq!(poster.posts.get(), 1);
}

#[derive(Debug)]
struct PostFailure;

impl fmt::Display for PostFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("post rejected")
    }
}

impl Error for PostFailure {}

struct FailOncePoster {
    posts: Cell<usize>,
    reject_next: Cell<bool>,
}

impl DrainPoster for FailOncePoster {
    type Error = PostFailure;

    fn post_drain(&self, _attachment: rustjsi_host::AttachmentId) -> Result<(), Self::Error> {
        self.posts.set(self.posts.get() + 1);
        if self.reject_next.replace(false) {
            Err(PostFailure)
        } else {
            Ok(())
        }
    }
}

#[test]
fn failed_post_preserves_pending_work_for_explicit_retry() {
    let host = ModelHost::new().unwrap();
    let mailbox = ScheduledWorkMailbox::new(NonZeroUsize::new(2).unwrap());
    let poster = FailOncePoster {
        posts: Cell::new(0),
        reject_next: Cell::new(true),
    };

    let error = mailbox
        .enqueue_and_post(&poster, host.attachment_id(), 1_u32)
        .expect_err("first post must fail");
    assert!(matches!(error, ScheduledWorkPostError::Post { .. }));
    assert_eq!(poster.posts.get(), 1);

    let _ = mailbox
        .enqueue_and_post(&poster, host.attachment_id(), 2_u32)
        .unwrap();
    assert_eq!(poster.posts.get(), 1);
    assert!(mailbox.post_pending(&poster, host.attachment_id()).unwrap());
    assert_eq!(poster.posts.get(), 2);

    let ScheduledWorkAcquire::Acquired(drain) = mailbox.acquire() else {
        panic!("pending work must remain drainable after retry");
    };
    assert_eq!(drain.pop().unwrap().into_payload(), 1);
    assert_eq!(drain.pop().unwrap().into_payload(), 2);
    let _ = drain.finish();
    assert!(!mailbox.post_pending(&poster, host.attachment_id()).unwrap());
}
