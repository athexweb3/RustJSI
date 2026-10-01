// SPDX-License-Identifier: MIT OR Apache-2.0

use std::error::Error;
use std::fmt;
use std::num::NonZeroUsize;
use std::sync::Arc;

use rustjsi_runtime::{ClosableMailboxState, IngressClose, MailboxEnqueue, TerminalAcquireError};

use crate::{
    AttachmentId, DrainPoster, DrainRegistration, DrainRegistrationReplaceError,
    DrainRegistrationState, DrainTaskResolution, ScheduledWorkEnqueueError, ScheduledWorkMailbox,
    ScheduledWorkPostError, TerminalScheduledWorkDrain,
};

/// Producer-shareable sender for one immutable attachment work mailbox.
///
/// A sender retains no host entry capability. It may enqueue payloads and
/// request an identity-only drain post, but it cannot resolve tasks, close
/// ingress, transfer terminal work, or replace the attachment.
#[derive(Clone, Debug)]
pub struct ScheduledWorkSender<T> {
    mailbox: Arc<ScheduledWorkMailbox<T>>,
}

/// Host-owner resolution of a future attachment drain task.
#[derive(Debug)]
#[must_use]
pub enum AttachmentWorkResolution<'owner, T> {
    /// The task targets the active current attachment and may use this mailbox.
    Current(&'owner ScheduledWorkMailbox<T>),
    /// The task targets the current attachment while it is closing.
    Closing,
    /// The task targets an earlier attachment of this logical runtime.
    Retired,
    /// The task targets a later or otherwise unregistered attachment epoch.
    UnregisteredAttachment,
    /// The task targets another logical runtime.
    ForeignRuntime,
}

/// Failure to replace the current attachment work mailbox.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AttachmentWorkReplaceError {
    /// The old mailbox still owns work, ingress, or terminal transition state.
    MailboxNotTerminallyClosed(ClosableMailboxState),
    /// The attachment registration rejected the replacement identity.
    Registration(DrainRegistrationReplaceError),
}

/// Thread-affine owner of one current attachment registration and mailbox.
///
/// The owner composes identity resolution with bounded work retention for one
/// logical runtime. It does not provide a platform scheduler, wake-up loop,
/// backend access, or host entry. Producers hold [`ScheduledWorkSender`] while
/// this owner resolves future task identities, closes work admission, transfers
/// terminal work, and installs replacement attachments.
///
/// ```compile_fail
/// use std::num::NonZeroUsize;
/// use rustjsi_host::{AttachmentWorkOwner, RuntimeIdentity};
///
/// fn requires_send<T: Send>(_: T) {}
///
/// let mut identity = RuntimeIdentity::allocate().unwrap();
/// let attachment = identity.next_attachment().unwrap();
/// requires_send(AttachmentWorkOwner::<()>::new(
///     attachment,
///     NonZeroUsize::new(1).unwrap(),
/// ));
/// ```
#[derive(Debug)]
pub struct AttachmentWorkOwner<T> {
    registration: DrainRegistration,
    mailbox: Arc<ScheduledWorkMailbox<T>>,
}

impl<T> AttachmentWorkOwner<T> {
    /// Creates an active owner with one current attachment mailbox.
    #[must_use]
    pub fn new(attachment: AttachmentId, capacity: NonZeroUsize) -> Self {
        Self {
            registration: DrainRegistration::new(attachment),
            mailbox: Arc::new(ScheduledWorkMailbox::new(attachment, capacity)),
        }
    }

    /// Returns the current attachment identity.
    #[must_use]
    pub fn attachment_id(&self) -> AttachmentId {
        self.registration.attachment_id()
    }

    /// Returns the current host-owner registration state.
    #[must_use]
    pub const fn state(&self) -> DrainRegistrationState {
        self.registration.state()
    }

    /// Returns a producer sender for the current immutable attachment mailbox.
    #[must_use]
    pub fn sender(&self) -> ScheduledWorkSender<T> {
        ScheduledWorkSender {
            mailbox: Arc::clone(&self.mailbox),
        }
    }

    /// Resolves one platform-delivered attachment task without host entry.
    ///
    /// A current resolution lends only the current mailbox. The immutable
    /// borrow prevents same-thread close or replacement for the duration of
    /// that resolution.
    pub fn resolve_drain_task(&self, attachment: AttachmentId) -> AttachmentWorkResolution<'_, T> {
        match self.registration.resolve(attachment) {
            DrainTaskResolution::Current => AttachmentWorkResolution::Current(&self.mailbox),
            DrainTaskResolution::Closing => AttachmentWorkResolution::Closing,
            DrainTaskResolution::Retired => AttachmentWorkResolution::Retired,
            DrainTaskResolution::UnregisteredAttachment => {
                AttachmentWorkResolution::UnregisteredAttachment
            }
            DrainTaskResolution::ForeignRuntime => AttachmentWorkResolution::ForeignRuntime,
        }
    }

    /// Stops current task dispatch and later producer admission.
    ///
    /// Registration becomes closing before mailbox ingress begins closing. A
    /// concurrent producer accepted in that interval retains terminal work but
    /// cannot create newly dispatchable current work.
    pub fn begin_close(&mut self) -> IngressClose {
        self.registration.begin_close();
        self.mailbox.begin_close()
    }

    /// Attempts an explicit retry post for currently retained work.
    ///
    /// This remains owner policy rather than a producer-sender capability.
    ///
    /// # Errors
    ///
    /// Returns the poster's failure while preserving the pending mailbox.
    pub fn post_pending<P>(&self, poster: &P) -> Result<bool, P::Error>
    where
        P: DrainPoster,
    {
        self.mailbox.post_pending(poster)
    }

    /// Attempts terminal ownership of the closing current mailbox.
    ///
    /// # Errors
    ///
    /// Returns outstanding ingress or drain ownership until terminal transfer
    /// is safe. Terminal work remains caller-owned through the returned drain.
    pub fn try_begin_terminal_drain(
        &self,
    ) -> Result<TerminalScheduledWorkDrain<'_, T>, TerminalAcquireError> {
        self.mailbox.try_begin_terminal_drain()
    }

    /// Installs a new mailbox after the old mailbox terminally closed.
    ///
    /// # Errors
    ///
    /// Rejects replacement until old terminal ownership completed, or when the
    /// underlying registration rejects the new attachment identity.
    pub fn replace(
        &mut self,
        replacement: AttachmentId,
        capacity: NonZeroUsize,
    ) -> Result<(), AttachmentWorkReplaceError> {
        let old_state = self.mailbox.state();
        if old_state != ClosableMailboxState::Closed {
            return Err(AttachmentWorkReplaceError::MailboxNotTerminallyClosed(
                old_state,
            ));
        }

        let replacement_mailbox = Arc::new(ScheduledWorkMailbox::new(replacement, capacity));
        self.registration
            .replace(replacement)
            .map_err(AttachmentWorkReplaceError::Registration)?;
        self.mailbox = replacement_mailbox;
        Ok(())
    }
}

impl<T> ScheduledWorkSender<T> {
    /// Returns the immutable attachment targeted by this sender.
    #[must_use]
    pub fn attachment_id(&self) -> AttachmentId {
        self.mailbox.attachment_id()
    }

    /// Enqueues one payload for this sender's immutable attachment.
    ///
    /// # Errors
    ///
    /// Returns the complete attachment-bound record if the mailbox is full or
    /// producer admission has closed.
    pub fn enqueue(&self, payload: T) -> Result<MailboxEnqueue, ScheduledWorkEnqueueError<T>> {
        self.mailbox.enqueue(payload)
    }

    /// Enqueues one payload and accepts its initial identity-only post.
    ///
    /// # Errors
    ///
    /// Returns the complete work record on admission failure. A post failure
    /// retains work in this sender's mailbox for owner-defined retry policy.
    pub fn enqueue_and_post<P>(
        &self,
        poster: &P,
        payload: T,
    ) -> Result<MailboxEnqueue, ScheduledWorkPostError<T, P::Error>>
    where
        P: DrainPoster,
    {
        self.mailbox.enqueue_and_post(poster, payload)
    }
}

impl fmt::Display for AttachmentWorkReplaceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MailboxNotTerminallyClosed(state) => {
                write!(
                    formatter,
                    "current mailbox is not terminally closed: {state:?}"
                )
            }
            Self::Registration(error) => {
                write!(formatter, "replacement registration failed: {error}")
            }
        }
    }
}

impl Error for AttachmentWorkReplaceError {}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;
    use std::num::NonZeroUsize;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier};
    use std::thread;

    use rustjsi_runtime::MailboxEnqueue;

    use super::*;
    use crate::{DrainPoster, RuntimeIdentity, ScheduledWorkAcquire};

    #[derive(Default)]
    struct CountingPoster {
        posts: AtomicUsize,
    }

    impl DrainPoster for CountingPoster {
        type Error = Infallible;

        fn post_drain(&self, _attachment: AttachmentId) -> Result<(), Self::Error> {
            self.posts.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
    }

    fn requires_send_sync<T: Send + Sync>() {}

    #[test]
    fn senders_with_send_payloads_are_producer_shareable() {
        requires_send_sync::<ScheduledWorkSender<u8>>();
    }

    #[test]
    fn close_terminal_drain_and_replacement_isolate_old_senders_and_tasks() {
        let mut identity = RuntimeIdentity::allocate().unwrap();
        let first = identity.next_attachment().unwrap();
        let replacement = identity.next_attachment().unwrap();
        let mut owner = AttachmentWorkOwner::new(first, NonZeroUsize::new(1).unwrap());
        let old_sender = owner.sender();

        assert!(matches!(
            old_sender.enqueue(7_u8),
            Ok(MailboxEnqueue::Scheduled)
        ));
        let _ = owner.begin_close();
        let Err(ScheduledWorkEnqueueError::Closed(work)) = old_sender.enqueue(8_u8) else {
            panic!("closing a mailbox must reject later producer work");
        };
        assert_eq!(work.into_payload(), 8);
        assert_eq!(
            owner.replace(replacement, NonZeroUsize::new(1).unwrap()),
            Err(AttachmentWorkReplaceError::MailboxNotTerminallyClosed(
                ClosableMailboxState::Closing
            ))
        );

        let terminal = owner.try_begin_terminal_drain().unwrap();
        assert_eq!(terminal.pop().unwrap().into_payload(), 7);
        terminal.finish().unwrap();
        owner
            .replace(replacement, NonZeroUsize::new(1).unwrap())
            .unwrap();

        assert_eq!(owner.attachment_id(), replacement);
        let Err(ScheduledWorkEnqueueError::Closed(work)) = old_sender.enqueue(9_u8) else {
            panic!("old senders must remain bound to the retired mailbox");
        };
        assert_eq!(work.into_payload(), 9);

        let replacement_sender = owner.sender();
        assert_eq!(replacement_sender.attachment_id(), replacement);
        assert!(matches!(
            replacement_sender.enqueue(41_u8),
            Ok(MailboxEnqueue::Scheduled)
        ));
        assert!(matches!(
            owner.resolve_drain_task(first),
            AttachmentWorkResolution::Retired
        ));
        let AttachmentWorkResolution::Current(mailbox) = owner.resolve_drain_task(replacement)
        else {
            panic!("replacement task must lend only the replacement mailbox");
        };
        let ScheduledWorkAcquire::Acquired(drain) = mailbox.acquire() else {
            panic!("replacement mailbox must retain replacement sender work");
        };
        assert_eq!(drain.pop().unwrap().into_payload(), 41);
        assert_eq!(drain.finish(), rustjsi_runtime::DrainAfter::Idle);
    }

    #[test]
    fn close_racing_producer_posts_preserve_terminal_payload_ownership() {
        const PRODUCERS: usize = 8;
        const ROUNDS: usize = if cfg!(miri) { 2 } else { 64 };

        for _ in 0..ROUNDS {
            let mut identity = RuntimeIdentity::allocate().unwrap();
            let attachment = identity.next_attachment().unwrap();
            let replacement = identity.next_attachment().unwrap();
            let mut owner =
                AttachmentWorkOwner::new(attachment, NonZeroUsize::new(PRODUCERS).unwrap());
            let old_sender = owner.sender();
            let poster = Arc::new(CountingPoster::default());
            let start = Arc::new(Barrier::new(PRODUCERS + 1));
            let mut workers = Vec::with_capacity(PRODUCERS);

            for payload in 0..u8::try_from(PRODUCERS).unwrap() {
                let sender = old_sender.clone();
                let poster = Arc::clone(&poster);
                let start = Arc::clone(&start);
                workers.push(thread::spawn(move || {
                    start.wait();
                    (payload, sender.enqueue_and_post(poster.as_ref(), payload))
                }));
            }

            start.wait();
            let _ = owner.begin_close();

            let mut accepted = Vec::with_capacity(PRODUCERS);
            let mut rejected = Vec::with_capacity(PRODUCERS);
            let mut scheduled = 0;
            for worker in workers {
                let (payload, result) = worker.join().expect("producer must not panic");
                match result {
                    Ok(MailboxEnqueue::Scheduled) => {
                        accepted.push(payload);
                        scheduled += 1;
                    }
                    Ok(MailboxEnqueue::Coalesced) => accepted.push(payload),
                    Err(ScheduledWorkPostError::Enqueue(ScheduledWorkEnqueueError::Closed(
                        work,
                    ))) => {
                        assert_eq!(work.attachment_id(), attachment);
                        assert_eq!(work.into_payload(), payload);
                        rejected.push(payload);
                    }
                    Err(ScheduledWorkPostError::Enqueue(ScheduledWorkEnqueueError::Full(work))) => {
                        panic!("close race must not exhaust producer-sized capacity: {work:?}")
                    }
                    Err(ScheduledWorkPostError::Post { error, .. }) => match error {},
                }
            }

            assert!(matches!(
                owner.resolve_drain_task(attachment),
                AttachmentWorkResolution::Closing
            ));
            let terminal = owner.try_begin_terminal_drain().unwrap();
            let mut terminal_payloads = Vec::with_capacity(PRODUCERS);
            while let Some(work) = terminal.pop() {
                assert_eq!(work.attachment_id(), attachment);
                terminal_payloads.push(work.into_payload());
            }
            terminal.finish().unwrap();

            accepted.sort_unstable();
            rejected.sort_unstable();
            terminal_payloads.sort_unstable();
            assert_eq!(terminal_payloads, accepted);
            let accepted_any = !accepted.is_empty();
            let mut accounted = accepted;
            accounted.extend(rejected);
            accounted.sort_unstable();
            assert_eq!(
                accounted,
                (0..u8::try_from(PRODUCERS).unwrap()).collect::<Vec<_>>()
            );
            assert_eq!(scheduled, usize::from(accepted_any));
            assert_eq!(
                poster.posts.load(Ordering::Relaxed),
                usize::from(accepted_any)
            );

            owner
                .replace(replacement, NonZeroUsize::new(PRODUCERS).unwrap())
                .unwrap();
            let Err(ScheduledWorkEnqueueError::Closed(work)) = old_sender.enqueue(u8::MAX) else {
                panic!("old sender must remain closed after replacement");
            };
            assert_eq!(work.attachment_id(), attachment);
            assert_eq!(work.into_payload(), u8::MAX);
            assert!(matches!(
                owner.resolve_drain_task(attachment),
                AttachmentWorkResolution::Retired
            ));
        }
    }
}
