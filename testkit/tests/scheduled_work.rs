// SPDX-License-Identifier: MIT OR Apache-2.0

//! Conformance tests for attachment-bound host work delivery.

use std::cell::Cell;
use std::convert::Infallible;
use std::error::Error;
use std::fmt;
use std::num::NonZeroUsize;

use rustjsi_host::{
    DrainPoster, Host, RuntimeIdentity, ScheduledWork, ScheduledWorkAcquire,
    ScheduledWorkFinishError, ScheduledWorkMailbox, ScheduledWorkPostError, WorkDispatchError,
};
use rustjsi_runtime::{DrainAfter, TerminalAcquireError};
use rustjsi_testkit::{DrainPostAcquire, DrainPostQueue, ModelHost};

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
    let mailbox = ScheduledWorkMailbox::new(host.attachment_id(), NonZeroUsize::new(1).unwrap());
    let _ = mailbox.enqueue(41_u32).unwrap();

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
    let mailbox = ScheduledWorkMailbox::new(stale, NonZeroUsize::new(1).unwrap());
    let _ = mailbox.enqueue(String::from("stale")).unwrap();
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
fn mailbox_drain_returns_work_when_host_entry_is_rejected() {
    let mut host = ModelHost::new().unwrap();
    let attachment = host.attachment_id();
    let mailbox = ScheduledWorkMailbox::new(attachment, NonZeroUsize::new(1).unwrap());
    let _ = mailbox
        .enqueue(vec![1_u8, 2, 3])
        .expect("active host attachment must enqueue");
    host.request_drain();

    let ScheduledWorkAcquire::Acquired(drain) = mailbox.acquire() else {
        panic!("queued work must acquire a normal drain");
    };
    let error = drain
        .dispatch_next(&mut host, |_, _| {
            panic!("draining host must not invoke work")
        })
        .expect_err("draining host must reject queued work");
    match error {
        WorkDispatchError::Entry { error, work } => {
            assert!(error.to_string().contains("not active"));
            assert_eq!(work.attachment_id(), attachment);
            assert_eq!(work.into_payload(), vec![1, 2, 3]);
        }
        other => panic!("expected host entry rejection, got {other:?}"),
    }
    assert_eq!(drain.finish(), rustjsi_runtime::DrainAfter::Idle);
}

#[test]
fn terminal_mailbox_drain_retains_stale_attachment_for_caller_policy() {
    let mut identity = RuntimeIdentity::allocate().unwrap();
    let stale = identity.next_attachment().unwrap();
    let current = identity.next_attachment().unwrap();
    let mailbox = ScheduledWorkMailbox::new(stale, NonZeroUsize::new(1).unwrap());
    let _ = mailbox.enqueue(String::from("stale")).unwrap();
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
    last_attachment: Cell<Option<rustjsi_host::AttachmentId>>,
}

impl DrainPoster for RecordingPoster {
    type Error = Infallible;

    fn post_drain(&self, attachment: rustjsi_host::AttachmentId) -> Result<(), Self::Error> {
        self.posts.set(self.posts.get() + 1);
        self.last_attachment.set(Some(attachment));
        Ok(())
    }
}

struct TerminalAttemptPoster<'mailbox> {
    mailbox: &'mailbox ScheduledWorkMailbox<u32>,
    posts: Cell<usize>,
    terminal_blocked: Cell<bool>,
}

impl DrainPoster for TerminalAttemptPoster<'_> {
    type Error = Infallible;

    fn post_drain(&self, _attachment: rustjsi_host::AttachmentId) -> Result<(), Self::Error> {
        self.posts.set(self.posts.get() + 1);
        let _ = self.mailbox.begin_close();
        self.terminal_blocked.set(matches!(
            self.mailbox.try_begin_terminal_drain(),
            Err(TerminalAcquireError::NormalDrainReservationsRemain(count)) if count.get() == 1
        ));
        Ok(())
    }
}

#[test]
fn mailbox_posts_once_for_a_coalesced_drain() {
    let host = ModelHost::new().unwrap();
    let attachment = host.attachment_id();
    let mailbox = ScheduledWorkMailbox::new(attachment, NonZeroUsize::new(2).unwrap());
    let poster = RecordingPoster::default();

    assert_eq!(mailbox.attachment_id(), attachment);

    let _ = mailbox.enqueue_and_post(&poster, 1_u32).unwrap();
    let _ = mailbox.enqueue_and_post(&poster, 2_u32).unwrap();

    assert_eq!(poster.posts.get(), 1);
    assert_eq!(poster.last_attachment.get(), Some(attachment));
}

#[test]
fn retry_post_holds_normal_admission_until_host_acceptance() {
    let host = ModelHost::new().unwrap();
    let mailbox = ScheduledWorkMailbox::new(host.attachment_id(), NonZeroUsize::new(1).unwrap());
    let poster = TerminalAttemptPoster {
        mailbox: &mailbox,
        posts: Cell::new(0),
        terminal_blocked: Cell::new(false),
    };
    let _ = mailbox.enqueue(7_u32).unwrap();

    assert!(mailbox.post_pending(&poster).unwrap());
    assert_eq!(poster.posts.get(), 1);
    assert!(poster.terminal_blocked.get());

    let terminal = mailbox.try_begin_terminal_drain().unwrap();
    assert_eq!(terminal.pop().unwrap().into_payload(), 7);
    terminal.finish().unwrap();
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
    let mailbox = ScheduledWorkMailbox::new(host.attachment_id(), NonZeroUsize::new(2).unwrap());
    let poster = FailOncePoster {
        posts: Cell::new(0),
        reject_next: Cell::new(true),
    };

    let error = mailbox
        .enqueue_and_post(&poster, 1_u32)
        .expect_err("first post must fail");
    assert!(matches!(error, ScheduledWorkPostError::Post { .. }));
    assert_eq!(poster.posts.get(), 1);

    let _ = mailbox.enqueue_and_post(&poster, 2_u32).unwrap();
    assert_eq!(poster.posts.get(), 1);
    assert!(mailbox.post_pending(&poster).unwrap());
    assert_eq!(poster.posts.get(), 2);

    let ScheduledWorkAcquire::Acquired(drain) = mailbox.acquire() else {
        panic!("pending work must remain drainable after retry");
    };
    assert_eq!(drain.pop().unwrap().into_payload(), 1);
    assert_eq!(drain.pop().unwrap().into_payload(), 2);
    let _ = drain.finish();
    assert!(!mailbox.post_pending(&poster).unwrap());
}

#[test]
fn finishing_a_pending_drain_posts_the_bound_successor() {
    let host = ModelHost::new().unwrap();
    let attachment = host.attachment_id();
    let mailbox = ScheduledWorkMailbox::new(attachment, NonZeroUsize::new(2).unwrap());
    let poster = RecordingPoster::default();

    let _ = mailbox.enqueue_and_post(&poster, 1_u32).unwrap();
    let ScheduledWorkAcquire::Acquired(drain) = mailbox.acquire() else {
        panic!("initial work must acquire a normal drain");
    };
    assert_eq!(drain.pop().unwrap().into_payload(), 1);
    assert!(matches!(
        mailbox.enqueue(2_u32),
        Ok(rustjsi_runtime::MailboxEnqueue::Coalesced)
    ));

    assert_eq!(drain.finish_and_post(&poster).unwrap(), DrainAfter::Pending);
    assert_eq!(poster.posts.get(), 2);
    assert_eq!(poster.last_attachment.get(), Some(attachment));

    let ScheduledWorkAcquire::Acquired(successor) = mailbox.acquire() else {
        panic!("pending finish must retain a successor drain");
    };
    assert_eq!(successor.pop().unwrap().into_payload(), 2);
    assert_eq!(
        successor.finish_and_post(&poster).unwrap(),
        DrainAfter::Idle
    );
    assert_eq!(poster.posts.get(), 2);
}

#[test]
fn failed_successor_post_keeps_mailbox_pending_for_retry() {
    let host = ModelHost::new().unwrap();
    let attachment = host.attachment_id();
    let mailbox = ScheduledWorkMailbox::new(attachment, NonZeroUsize::new(2).unwrap());
    let poster = FailOncePoster {
        posts: Cell::new(0),
        reject_next: Cell::new(true),
    };

    let _ = mailbox.enqueue(1_u32).unwrap();
    let ScheduledWorkAcquire::Acquired(drain) = mailbox.acquire() else {
        panic!("initial work must acquire a normal drain");
    };
    assert_eq!(drain.pop().unwrap().into_payload(), 1);
    assert!(matches!(
        mailbox.enqueue(2_u32),
        Ok(rustjsi_runtime::MailboxEnqueue::Coalesced)
    ));

    let error = drain
        .finish_and_post(&poster)
        .expect_err("first successor post must fail");
    assert!(matches!(
        error,
        ScheduledWorkFinishError::Post {
            attachment: error_attachment,
            ..
        } if error_attachment == attachment
    ));
    assert_eq!(poster.posts.get(), 1);

    assert!(mailbox.post_pending(&poster).unwrap());
    assert_eq!(poster.posts.get(), 2);
    let ScheduledWorkAcquire::Acquired(retry) = mailbox.acquire() else {
        panic!("failed successor post must leave work drainable after retry");
    };
    assert_eq!(retry.pop().unwrap().into_payload(), 2);
    assert_eq!(retry.finish(), DrainAfter::Idle);
}

#[test]
fn drain_post_queue_routes_initial_and_successor_work_drains() {
    let mut host = ModelHost::new().unwrap();
    let attachment = host.attachment_id();
    let mailbox = ScheduledWorkMailbox::new(attachment, NonZeroUsize::new(2).unwrap());
    let posts = DrainPostQueue::new(NonZeroUsize::new(2).unwrap());

    let _ = mailbox.enqueue_and_post(&posts, 1_u32).unwrap();
    let DrainPostAcquire::Acquired(initial_post) = posts.acquire() else {
        panic!("initial work must retain an attachment-only post");
    };
    assert_eq!(initial_post.pop(), Some(attachment));
    assert_eq!(initial_post.finish(), DrainAfter::Idle);

    let ScheduledWorkAcquire::Acquired(initial_drain) = mailbox.acquire() else {
        panic!("initial attachment post must make work drainable");
    };
    assert_eq!(
        initial_drain
            .dispatch_next(&mut host, |_, payload| payload + 1)
            .unwrap(),
        Some(2)
    );
    assert!(matches!(
        mailbox.enqueue(2_u32),
        Ok(rustjsi_runtime::MailboxEnqueue::Coalesced)
    ));
    assert_eq!(
        initial_drain.finish_and_post(&posts).unwrap(),
        DrainAfter::Pending
    );

    let DrainPostAcquire::Acquired(successor_post) = posts.acquire() else {
        panic!("pending work must retain one successor attachment post");
    };
    assert_eq!(successor_post.pop(), Some(attachment));
    assert_eq!(successor_post.finish(), DrainAfter::Idle);

    let ScheduledWorkAcquire::Acquired(successor_drain) = mailbox.acquire() else {
        panic!("successor attachment post must preserve pending work");
    };
    assert_eq!(
        successor_drain
            .dispatch_next(&mut host, |_, payload| payload + 1)
            .unwrap(),
        Some(3)
    );
    assert_eq!(successor_drain.finish(), DrainAfter::Idle);
}

#[test]
fn full_drain_post_queue_preserves_work_for_retry() {
    let mut host = ModelHost::new().unwrap();
    let attachment = host.attachment_id();
    let mailbox = ScheduledWorkMailbox::new(attachment, NonZeroUsize::new(1).unwrap());
    let posts = DrainPostQueue::new(NonZeroUsize::new(1).unwrap());
    let mut other_identity = RuntimeIdentity::allocate().unwrap();
    let other_attachment = other_identity.next_attachment().unwrap();

    posts.post_drain(other_attachment).unwrap();
    let error = mailbox
        .enqueue_and_post(&posts, 41_u32)
        .expect_err("a full attachment-post queue must reject the initial post");
    assert!(matches!(
        error,
        ScheduledWorkPostError::Post {
            attachment: error_attachment,
            error,
        } if error_attachment == attachment && error.attachment_id() == attachment
    ));

    let DrainPostAcquire::Acquired(blocking_post) = posts.acquire() else {
        panic!("the earlier attachment post must remain owned by the queue");
    };
    assert_eq!(blocking_post.pop(), Some(other_attachment));
    assert_eq!(blocking_post.finish(), DrainAfter::Idle);

    assert!(mailbox.post_pending(&posts).unwrap());
    let DrainPostAcquire::Acquired(retry_post) = posts.acquire() else {
        panic!("retry must retain a fresh attachment post");
    };
    assert_eq!(retry_post.pop(), Some(attachment));
    assert_eq!(retry_post.finish(), DrainAfter::Idle);

    let ScheduledWorkAcquire::Acquired(drain) = mailbox.acquire() else {
        panic!("post rejection must retain the queued work for retry");
    };
    assert_eq!(
        drain
            .dispatch_next(&mut host, |_, payload| payload + 1)
            .unwrap(),
        Some(42)
    );
    assert_eq!(drain.finish(), DrainAfter::Idle);
}
