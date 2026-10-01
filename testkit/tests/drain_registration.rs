// SPDX-License-Identifier: MIT OR Apache-2.0

//! Conformance checks for deterministic host-owned task registration and work ownership.

use std::collections::VecDeque;
use std::num::NonZeroUsize;

use rustjsi_host::{
    AttachmentId, AttachmentWorkOwner, AttachmentWorkReplaceError, AttachmentWorkResolution,
    DrainPoster, DrainRegistration, DrainRegistrationReplaceError, DrainRegistrationState,
    DrainTaskResolution, RuntimeIdentity, ScheduledWorkAcquire, ScheduledWorkEnqueueError,
    ScheduledWorkMailbox, ScheduledWorkSender,
};
use rustjsi_runtime::{ClosableMailboxState, DrainAfter, MailboxEnqueue, TerminalAcquireError};
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

const OWNER_SEQUENCE_COUNT: usize = if cfg!(miri) { 40 } else { 20_000 };
const OWNER_STEPS_PER_SEQUENCE: usize = 32;
const OWNER_WORK_CAPACITY: usize = 2;
const OWNER_ATTACHMENT_COUNT: usize = 8;
const OWNER_SEQUENCE_SEED: u64 = 0x0a11_ce55_0f13_1d5e;

/// Small deterministic selector for owner operation sequences.
struct OwnerRandom(u64);

impl OwnerRandom {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut value = self.0;
        value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        value ^ (value >> 31)
    }

    fn below(&mut self, bound: usize) -> usize {
        let bound = u64::try_from(bound).unwrap();
        usize::try_from(self.next() % bound).unwrap()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OwnerMailboxPhase {
    Open,
    Closing,
    Closed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OwnerTaskResolution {
    Current,
    Closing,
    Retired,
    UnregisteredAttachment,
    ForeignRuntime,
}

#[derive(Debug)]
struct OwnerReference {
    current: AttachmentId,
    registration: DrainRegistrationState,
    mailbox: OwnerMailboxPhase,
    retained: VecDeque<u16>,
}

impl OwnerReference {
    fn new(current: AttachmentId) -> Self {
        Self {
            current,
            registration: DrainRegistrationState::Active,
            mailbox: OwnerMailboxPhase::Open,
            retained: VecDeque::new(),
        }
    }

    fn begin_close(&mut self) {
        self.registration = DrainRegistrationState::Closing;
        if self.mailbox == OwnerMailboxPhase::Open {
            self.mailbox = OwnerMailboxPhase::Closing;
        }
    }

    fn resolve(&self, attachment: AttachmentId) -> OwnerTaskResolution {
        if attachment.runtime_id() != self.current.runtime_id() {
            OwnerTaskResolution::ForeignRuntime
        } else if attachment.epoch() < self.current.epoch() {
            OwnerTaskResolution::Retired
        } else if attachment != self.current {
            OwnerTaskResolution::UnregisteredAttachment
        } else {
            match self.registration {
                DrainRegistrationState::Active => OwnerTaskResolution::Current,
                DrainRegistrationState::Closing => OwnerTaskResolution::Closing,
            }
        }
    }

    fn replacement_result(
        &self,
        replacement: AttachmentId,
    ) -> Result<(), AttachmentWorkReplaceError> {
        match self.mailbox {
            OwnerMailboxPhase::Open => Err(AttachmentWorkReplaceError::MailboxNotTerminallyClosed(
                ClosableMailboxState::Open,
            )),
            OwnerMailboxPhase::Closing => {
                Err(AttachmentWorkReplaceError::MailboxNotTerminallyClosed(
                    ClosableMailboxState::Closing,
                ))
            }
            OwnerMailboxPhase::Closed => {
                if replacement.runtime_id() != self.current.runtime_id() {
                    Err(AttachmentWorkReplaceError::Registration(
                        DrainRegistrationReplaceError::ForeignRuntime,
                    ))
                } else if replacement.epoch() <= self.current.epoch() {
                    Err(AttachmentWorkReplaceError::Registration(
                        DrainRegistrationReplaceError::NotNewer,
                    ))
                } else {
                    Ok(())
                }
            }
        }
    }

    fn replace(&mut self, replacement: AttachmentId) {
        debug_assert!(self.retained.is_empty());
        self.current = replacement;
        self.registration = DrainRegistrationState::Active;
        self.mailbox = OwnerMailboxPhase::Open;
    }
}

#[derive(Debug)]
struct RecordedSender {
    attachment: AttachmentId,
    sender: ScheduledWorkSender<u16>,
}

#[derive(Default)]
struct OwnerSequenceCoverage {
    accepted: usize,
    full: usize,
    closed: usize,
    terminal_open_rejections: usize,
    terminal_successes: usize,
    terminal_closed_rejections: usize,
    replacement_successes: usize,
    mailbox_rejections: usize,
    registration_rejections: usize,
    current_resolutions: usize,
    closing_resolutions: usize,
    retired_resolutions: usize,
    unregistered_resolutions: usize,
    foreign_resolutions: usize,
}

fn observed_resolution(resolution: &AttachmentWorkResolution<'_, u16>) -> OwnerTaskResolution {
    match resolution {
        AttachmentWorkResolution::Current(_) => OwnerTaskResolution::Current,
        AttachmentWorkResolution::Closing => OwnerTaskResolution::Closing,
        AttachmentWorkResolution::Retired => OwnerTaskResolution::Retired,
        AttachmentWorkResolution::UnregisteredAttachment => {
            OwnerTaskResolution::UnregisteredAttachment
        }
        AttachmentWorkResolution::ForeignRuntime => OwnerTaskResolution::ForeignRuntime,
    }
}

fn record_resolution(coverage: &mut OwnerSequenceCoverage, resolution: OwnerTaskResolution) {
    match resolution {
        OwnerTaskResolution::Current => coverage.current_resolutions += 1,
        OwnerTaskResolution::Closing => coverage.closing_resolutions += 1,
        OwnerTaskResolution::Retired => coverage.retired_resolutions += 1,
        OwnerTaskResolution::UnregisteredAttachment => coverage.unregistered_resolutions += 1,
        OwnerTaskResolution::ForeignRuntime => coverage.foreign_resolutions += 1,
    }
}

fn enqueue_through_recorded_sender(
    random: &mut OwnerRandom,
    step: usize,
    reference: &mut OwnerReference,
    senders: &[RecordedSender],
    coverage: &mut OwnerSequenceCoverage,
) {
    let sender = &senders[random.below(senders.len())];
    let payload = u16::try_from(step).unwrap();
    let result = sender.sender.enqueue(payload);
    if sender.attachment == reference.current
        && reference.mailbox == OwnerMailboxPhase::Open
        && reference.retained.len() < OWNER_WORK_CAPACITY
    {
        let expected = if reference.retained.is_empty() {
            MailboxEnqueue::Scheduled
        } else {
            MailboxEnqueue::Coalesced
        };
        let Ok(actual) = result else {
            panic!("an admissible current sender must retain its payload");
        };
        assert_eq!(actual, expected);
        reference.retained.push_back(payload);
        coverage.accepted += 1;
    } else if sender.attachment == reference.current && reference.mailbox == OwnerMailboxPhase::Open
    {
        let Err(ScheduledWorkEnqueueError::Full(work)) = result else {
            panic!("a full current mailbox must return its exact work record");
        };
        assert_eq!(work.attachment_id(), sender.attachment);
        assert_eq!(work.into_payload(), payload);
        coverage.full += 1;
    } else {
        let Err(ScheduledWorkEnqueueError::Closed(work)) = result else {
            panic!("retired or closing sender work must be rejected");
        };
        assert_eq!(work.attachment_id(), sender.attachment);
        assert_eq!(work.into_payload(), payload);
        coverage.closed += 1;
    }
}

fn transfer_terminal_work(
    owner: &AttachmentWorkOwner<u16>,
    reference: &mut OwnerReference,
    coverage: &mut OwnerSequenceCoverage,
) {
    match reference.mailbox {
        OwnerMailboxPhase::Open => {
            assert!(matches!(
                owner.try_begin_terminal_drain(),
                Err(TerminalAcquireError::ProducerCloseNotStarted)
            ));
            coverage.terminal_open_rejections += 1;
        }
        OwnerMailboxPhase::Closing => {
            let terminal = owner.try_begin_terminal_drain().unwrap();
            while let Some(expected) = reference.retained.pop_front() {
                let work = terminal
                    .pop()
                    .expect("terminal owner must retain every queued payload");
                assert_eq!(work.attachment_id(), reference.current);
                assert_eq!(work.into_payload(), expected);
            }
            assert!(terminal.pop().is_none());
            terminal.finish().unwrap();
            reference.mailbox = OwnerMailboxPhase::Closed;
            coverage.terminal_successes += 1;
        }
        OwnerMailboxPhase::Closed => {
            assert!(matches!(
                owner.try_begin_terminal_drain(),
                Err(TerminalAcquireError::Closed)
            ));
            coverage.terminal_closed_rejections += 1;
        }
    }
}

fn replace_attachment(
    random: &mut OwnerRandom,
    owner: &mut AttachmentWorkOwner<u16>,
    reference: &mut OwnerReference,
    attachments: &[AttachmentId],
    foreign: AttachmentId,
    capacity: NonZeroUsize,
    coverage: &mut OwnerSequenceCoverage,
) {
    let replacement = if random.below(5) == 0 {
        foreign
    } else {
        attachments[random.below(attachments.len())]
    };
    let expected = reference.replacement_result(replacement);
    let actual = owner.replace(replacement, capacity);
    assert_eq!(actual, expected);
    match actual {
        Ok(()) => {
            reference.replace(replacement);
            coverage.replacement_successes += 1;
        }
        Err(AttachmentWorkReplaceError::MailboxNotTerminallyClosed(_)) => {
            coverage.mailbox_rejections += 1;
        }
        Err(AttachmentWorkReplaceError::Registration(_)) => {
            coverage.registration_rejections += 1;
        }
    }
}

fn resolve_attachment_task(
    random: &mut OwnerRandom,
    owner: &AttachmentWorkOwner<u16>,
    reference: &OwnerReference,
    attachments: &[AttachmentId],
    foreign: AttachmentId,
    coverage: &mut OwnerSequenceCoverage,
) {
    let attachment = if random.below(5) == 0 {
        foreign
    } else {
        attachments[random.below(attachments.len())]
    };
    let expected = reference.resolve(attachment);
    let resolution = owner.resolve_drain_task(attachment);
    let actual = observed_resolution(&resolution);
    assert_eq!(actual, expected);
    record_resolution(coverage, actual);
}

#[test]
fn attachment_work_owner_sequences_match_reference() {
    let mut random = OwnerRandom(OWNER_SEQUENCE_SEED);
    let mut coverage = OwnerSequenceCoverage::default();
    let capacity = NonZeroUsize::new(OWNER_WORK_CAPACITY).unwrap();

    for _ in 0..OWNER_SEQUENCE_COUNT {
        let mut identity = RuntimeIdentity::allocate().unwrap();
        let attachments = (0..OWNER_ATTACHMENT_COUNT)
            .map(|_| identity.next_attachment().unwrap())
            .collect::<Vec<_>>();
        let foreign = RuntimeIdentity::allocate()
            .unwrap()
            .next_attachment()
            .unwrap();
        let mut owner = AttachmentWorkOwner::new(attachments[0], capacity);
        let mut reference = OwnerReference::new(attachments[0]);
        let mut senders = vec![RecordedSender {
            attachment: attachments[0],
            sender: owner.sender(),
        }];

        for step in 0..OWNER_STEPS_PER_SEQUENCE {
            match random.below(11) {
                0 => {
                    senders.push(RecordedSender {
                        attachment: reference.current,
                        sender: owner.sender(),
                    });
                }
                1..=4 => enqueue_through_recorded_sender(
                    &mut random,
                    step,
                    &mut reference,
                    &senders,
                    &mut coverage,
                ),
                5 => {
                    let _ = owner.begin_close();
                    reference.begin_close();
                    assert_eq!(owner.state(), reference.registration);
                }
                6 => transfer_terminal_work(&owner, &mut reference, &mut coverage),
                7 => replace_attachment(
                    &mut random,
                    &mut owner,
                    &mut reference,
                    &attachments,
                    foreign,
                    capacity,
                    &mut coverage,
                ),
                _ => resolve_attachment_task(
                    &mut random,
                    &owner,
                    &reference,
                    &attachments,
                    foreign,
                    &mut coverage,
                ),
            }

            assert_eq!(owner.attachment_id(), reference.current);
            assert_eq!(owner.state(), reference.registration);
        }
    }

    assert!(coverage.accepted > 0);
    assert!(coverage.full > 0);
    assert!(coverage.closed > 0);
    assert!(coverage.terminal_open_rejections > 0);
    assert!(coverage.terminal_successes > 0);
    assert!(coverage.terminal_closed_rejections > 0);
    assert!(coverage.replacement_successes > 0);
    assert!(coverage.mailbox_rejections > 0);
    assert!(coverage.registration_rejections > 0);
    assert!(coverage.current_resolutions > 0);
    assert!(coverage.closing_resolutions > 0);
    assert!(coverage.retired_resolutions > 0);
    assert!(coverage.unregistered_resolutions > 0);
    assert!(coverage.foreign_resolutions > 0);
}
