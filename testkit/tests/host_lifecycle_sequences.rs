// SPDX-License-Identifier: MIT OR Apache-2.0

//! Randomized multi-runtime lifecycle sequences against an independent
//! reference model.
//!
//! Each sequence allocates several logical runtimes and drives a seeded mix of
//! entry, exit, work-record issue, record delivery, invalidation, drain,
//! destruction, engine replacement, and abandoned creation. Replacement
//! invalidates the current attachment and installs a new epoch for the same
//! runtime; the retired attachment stays reachable so late exits, drains,
//! destruction, and records still reach it. Abandoned creation issues an
//! attachment identity that is never activated, so its epoch is consumed and
//! records carrying it can never find an owner. Records are delivered to their
//! own attachment, to a newer epoch of the same runtime, to other runtimes, and
//! after teardown.
//!
//! Every delivery is checked on two independent paths. The lifecycle model
//! validates the record identity and state. The model host is driven the way
//! a dispatcher would use the `Host` contract: the record identity is compared
//! with `Host::attachment_id`, and on a match `Host::with_backend` is called
//! unconditionally, whatever state the reference expects. The host's own entry
//! gate must therefore refuse the loan while draining, invalid, or destroyed,
//! and the loan counter proves the backend closure never ran. The `Host`
//! contract has no work-record admission method, so wrong-runtime and
//! wrong-epoch rejection on the host path is the identity comparison, not a
//! host-internal check.
//!
//! After every step each attachment's lifecycle model and model host must
//! agree with the reference: state, entry count, trace cardinality, monotonic
//! state order, and the number of backend loans. Because every attachment is
//! checked after every step, an operation that leaked into another runtime's
//! state would be detected. The attachments share no state by construction and
//! run on one thread, so this is not a concurrency test.
//!
//! The lifecycle starts in the active state: the model and host have no
//! separate created-but-not-activated state, so "create" means issuing an
//! attachment identity and installing it.
//!
//! The normal build runs 100,000 sequences of 24 steps over 3 runtimes
//! (2.4 million operations). Miri runs a reduced count with the same shape.

use rustjsi_host::{AttachmentId, GateError, Host, HostState, RuntimeIdentity};
use rustjsi_testkit::{
    Entry, LifecycleError, LifecycleEvent, LifecycleModel, ModelHost, RuntimeState,
};

const SEQUENCES: u32 = if cfg!(miri) { 40 } else { 100_000 };
const STEPS_PER_SEQUENCE: u32 = 24;
const RUNTIMES: usize = 3;
const SEED: u64 = 0x5eed_0f11_fec7_c1e5;

/// `SplitMix64`: small, deterministic, and sufficient for schedule selection.
struct Random(u64);

impl Random {
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

    fn chance(&mut self, percent: u64) -> bool {
        self.next() % 100 < percent
    }
}

/// Reference lifecycle phase, ordered so monotonicity is a comparison.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum Phase {
    Active,
    Draining,
    Invalid,
    Destroyed,
}

impl Phase {
    const fn model_state(self) -> RuntimeState {
        match self {
            Self::Active => RuntimeState::Active,
            Self::Draining => RuntimeState::Draining,
            Self::Invalid => RuntimeState::Invalid,
            Self::Destroyed => RuntimeState::Destroyed,
        }
    }

    const fn host_state(self) -> HostState {
        match self {
            Self::Active => HostState::Active,
            Self::Draining => HostState::Draining,
            Self::Invalid => HostState::Invalid,
            Self::Destroyed => HostState::Destroyed,
        }
    }
}

const fn observed_phase(state: RuntimeState) -> Phase {
    match state {
        RuntimeState::Active => Phase::Active,
        RuntimeState::Draining => Phase::Draining,
        RuntimeState::Invalid => Phase::Invalid,
        RuntimeState::Destroyed => Phase::Destroyed,
    }
}

/// Independent expectation for one attachment, kept as plain integers.
#[derive(Debug)]
struct Reference {
    runtime: u64,
    epoch: u64,
    phase: Phase,
    entries: u32,
    lends: u64,
}

impl Reference {
    fn validate(&self, record: &Record) -> Result<(), LifecycleError> {
        if record.runtime != self.runtime || record.epoch != self.epoch {
            Err(LifecycleError::WrongRuntime)
        } else if self.phase == Phase::Active {
            Ok(())
        } else {
            Err(LifecycleError::NotActive(self.phase.model_state()))
        }
    }

    fn invalidate(&mut self) {
        if self.phase == Phase::Active {
            self.phase = Phase::Draining;
        }
    }

    fn finish_drain(&mut self) -> Result<(), LifecycleError> {
        match self.phase {
            Phase::Invalid => Ok(()),
            Phase::Draining if self.entries == 0 => {
                self.phase = Phase::Invalid;
                Ok(())
            }
            Phase::Draining => Err(LifecycleError::EntriesRemain(self.entries)),
            phase => Err(LifecycleError::InvalidTransition {
                state: phase.model_state(),
                operation: "finish drain",
            }),
        }
    }

    fn destroy(&mut self) -> Result<(), LifecycleError> {
        match self.phase {
            Phase::Invalid | Phase::Destroyed => {
                self.phase = Phase::Destroyed;
                Ok(())
            }
            phase => Err(LifecycleError::InvalidTransition {
                state: phase.model_state(),
                operation: "destroy",
            }),
        }
    }
}

/// Queued work captured at issue time with the reference's raw identity.
#[derive(Clone, Copy, Debug)]
struct Record {
    id: AttachmentId,
    runtime: u64,
    epoch: u64,
}

struct Attachment {
    model: LifecycleModel,
    host: ModelHost,
    entries: Vec<Entry>,
    lends: u64,
    observed: Phase,
    trace: TraceCount,
    reference: Reference,
}

/// Terminal transitions observed so far in one model trace.
#[derive(Debug, Default)]
struct TraceCount {
    seen: usize,
    drain_requested: usize,
    invalidated: usize,
    destroyed: usize,
}

impl Attachment {
    fn new(id: AttachmentId, runtime: u64, epoch: u64) -> Self {
        Self {
            model: LifecycleModel::new(id),
            host: ModelHost::for_attachment(id),
            entries: Vec::new(),
            lends: 0,
            observed: Phase::Active,
            trace: TraceCount::default(),
            reference: Reference {
                runtime,
                epoch,
                phase: Phase::Active,
                entries: 0,
                lends: 0,
            },
        }
    }

    fn record(&self) -> Record {
        Record {
            id: self.model.attachment_id(),
            runtime: self.reference.runtime,
            epoch: self.reference.epoch,
        }
    }

    fn enter(&mut self) {
        let expected = self.reference.phase == Phase::Active;
        let actual = self.model.enter();
        assert_eq!(actual.is_ok(), expected, "entry admission diverged");
        if let Ok(entry) = actual {
            assert_eq!(entry.attachment_id(), self.model.attachment_id());
            self.entries.push(entry);
            self.reference.entries += 1;
        } else {
            assert_eq!(
                actual,
                Err(LifecycleError::NotActive(
                    self.reference.phase.model_state()
                ))
            );
        }
    }

    fn exit(&mut self, random: &mut Random) {
        if self.entries.is_empty() {
            return;
        }
        let entry = self.entries.swap_remove(random.below(self.entries.len()));
        assert_eq!(self.model.exit(entry), Ok(()));
        self.reference.entries -= 1;
        assert_eq!(self.model.exit(entry), Err(LifecycleError::StaleEntry));
    }

    /// Delivers one record through the lifecycle model and the host path.
    ///
    /// The host path does not consult the reference: on an identity match it
    /// always asks the host to lend its backend, so only the host's entry gate
    /// can refuse late work.
    fn deliver(&mut self, record: &Record, stats: &mut Coverage) {
        let expected = self.reference.validate(record);
        let actual = self.model.validate_work(record.id);
        assert_eq!(actual, expected, "record {record:?} diverged");

        let host_result = if record.id == self.host.attachment_id() {
            let lends = &mut self.lends;
            Some(self.host.with_backend(|_| *lends += 1))
        } else {
            None
        };

        match expected {
            Ok(()) => {
                assert_eq!(host_result, Some(Ok(())), "host refused valid work");
                self.reference.lends += 1;
                stats.accepted += 1;
            }
            Err(LifecycleError::WrongRuntime) => {
                assert_eq!(host_result, None, "host identity accepted {record:?}");
                if record.runtime == self.reference.runtime {
                    stats.epoch_mismatch += 1;
                } else {
                    stats.wrong_runtime += 1;
                }
            }
            Err(error) => {
                let phase = self.reference.phase;
                assert_eq!(error, LifecycleError::NotActive(phase.model_state()));
                assert_eq!(
                    host_result,
                    Some(Err(GateError::NotActive(phase.host_state()))),
                    "host gate admitted late work in {phase:?}"
                );
                match phase {
                    Phase::Draining => stats.gate_draining += 1,
                    Phase::Invalid => stats.gate_invalid += 1,
                    Phase::Destroyed => stats.gate_destroyed += 1,
                    Phase::Active => unreachable!("active work was rejected"),
                }
            }
        }
    }

    fn invalidate(&mut self, random: &mut Random) {
        let repeats = 1 + random.below(2);
        for _ in 0..repeats {
            self.model.request_invalidate();
            self.host.request_drain();
        }
        self.reference.invalidate();
    }

    fn finish_drain(&mut self) {
        let before = self.reference.phase;
        let expected = self.reference.finish_drain();
        assert_eq!(self.model.finish_drain(), expected);
        // The model host only counts entries it lends inside one step, so it
        // is driven only where its own gate sees the same legality.
        match (before, expected) {
            (_, Ok(())) => {
                self.host.finish_drain().unwrap();
            }
            (Phase::Active | Phase::Destroyed, Err(_)) => {
                assert!(self.host.finish_drain().is_err());
            }
            (_, Err(_)) => {}
        }
    }

    fn destroy(&mut self) {
        let expected = self.reference.destroy();
        assert_eq!(self.model.destroy(), expected);
        assert_eq!(self.host.mark_destroyed().is_ok(), expected.is_ok());
    }

    fn teardown(&mut self) {
        while let Some(entry) = self.entries.pop() {
            self.model.exit(entry).unwrap();
            self.reference.entries -= 1;
        }
        self.model.request_invalidate();
        self.host.request_drain();
        self.reference.invalidate();
        self.finish_drain();
        self.destroy();
        self.check();
        assert_eq!(self.reference.phase, Phase::Destroyed);
    }

    /// Compares implementation state with the reference after every step.
    fn check(&mut self) {
        let phase = self.reference.phase;
        let observed = observed_phase(self.model.state());
        assert_eq!(observed, phase, "model state diverged");
        assert!(observed >= self.observed, "lifecycle state moved backward");
        self.observed = observed;
        assert_eq!(self.host.state(), phase.host_state(), "host state diverged");
        assert_eq!(self.host.attachment_id(), self.model.attachment_id());
        assert_eq!(self.model.active_entries(), self.reference.entries);
        assert_eq!(
            u32::try_from(self.entries.len()).unwrap(),
            self.reference.entries
        );
        assert_eq!(
            self.lends, self.reference.lends,
            "backend lent unexpectedly"
        );

        // The trace is append-only; count only the transitions added since
        // the previous check.
        let events = self.model.events();
        assert!(events.len() >= self.trace.seen, "trace was rewritten");
        for event in &events[self.trace.seen..] {
            match event {
                LifecycleEvent::DrainRequested => self.trace.drain_requested += 1,
                LifecycleEvent::Invalidated => self.trace.invalidated += 1,
                LifecycleEvent::Destroyed => self.trace.destroyed += 1,
                _ => {}
            }
        }
        self.trace.seen = events.len();
        assert_eq!(
            self.trace.drain_requested,
            usize::from(phase >= Phase::Draining),
            "invalidation was not idempotent"
        );
        assert_eq!(self.trace.invalidated, usize::from(phase >= Phase::Invalid));
        assert_eq!(self.trace.destroyed, usize::from(phase == Phase::Destroyed));
    }
}

/// One logical runtime; the last attachment is current, earlier ones retired.
struct Runtime {
    identity: RuntimeIdentity,
    raw_id: u64,
    next_epoch: u64,
    attachments: Vec<Attachment>,
}

impl Runtime {
    fn new() -> Self {
        let identity = RuntimeIdentity::allocate().unwrap();
        let raw_id = identity.runtime_id().get();
        let mut this = Self {
            identity,
            raw_id,
            next_epoch: 1,
            attachments: Vec::new(),
        };
        this.install();
        this
    }

    fn install(&mut self) {
        let id = self.identity.next_attachment().unwrap();
        assert_eq!(id.runtime_id().get(), self.raw_id);
        assert_eq!(id.epoch().get(), self.next_epoch, "epoch was not fresh");
        self.attachments
            .push(Attachment::new(id, self.raw_id, self.next_epoch));
        self.next_epoch += 1;
    }

    /// Issues an attachment identity whose installation fails before
    /// activation. The epoch stays consumed, so the next install must skip it
    /// and the returned record can never match any attachment.
    fn abandon(&mut self, stats: &mut Coverage) -> Record {
        let id = self.identity.next_attachment().unwrap();
        assert_eq!(id.runtime_id().get(), self.raw_id);
        assert_eq!(id.epoch().get(), self.next_epoch, "epoch was not fresh");
        self.next_epoch += 1;
        stats.abandoned += 1;
        Record {
            id,
            runtime: self.raw_id,
            epoch: id.epoch().get(),
        }
    }

    fn current(&mut self) -> &mut Attachment {
        self.attachments.last_mut().unwrap()
    }

    fn any(&mut self, random: &mut Random) -> &mut Attachment {
        let index = random.below(self.attachments.len());
        &mut self.attachments[index]
    }

    /// Replaces the engine: the current attachment is invalidated and the
    /// same logical runtime receives the next epoch.
    fn replace(&mut self, random: &mut Random, stats: &mut Coverage) {
        self.current().invalidate(random);
        self.install();
        stats.replacements += 1;
    }
}

#[derive(Debug, Default)]
struct Coverage {
    accepted: u64,
    epoch_mismatch: u64,
    wrong_runtime: u64,
    gate_draining: u64,
    gate_invalid: u64,
    gate_destroyed: u64,
    replacements: u64,
    abandoned: u64,
    wrong_exit: u64,
}

fn foreign_record() -> Record {
    let mut identity = RuntimeIdentity::allocate().unwrap();
    let id = identity.next_attachment().unwrap();
    Record {
        id,
        runtime: identity.runtime_id().get(),
        epoch: 1,
    }
}

fn deliver_somewhere(
    runtimes: &mut [Runtime],
    record: &Record,
    random: &mut Random,
    stats: &mut Coverage,
) {
    let own = runtimes
        .iter()
        .position(|runtime| runtime.raw_id == record.runtime);
    let target = match own {
        Some(index) if random.chance(60) => runtimes[index].current(),
        _ => {
            let index = random.below(runtimes.len());
            runtimes[index].any(random)
        }
    };
    target.deliver(record, stats);
}

fn exit_on_wrong_attachment(runtimes: &mut [Runtime], random: &mut Random, stats: &mut Coverage) {
    let from = random.below(runtimes.len());
    let Some(entry) = runtimes[from].any(random).entries.last().copied() else {
        return;
    };
    let to = random.below(runtimes.len());
    let target = runtimes[to].any(random);
    if target.model.attachment_id() == entry.attachment_id() {
        return;
    }
    assert_eq!(target.model.exit(entry), Err(LifecycleError::WrongRuntime));
    stats.wrong_exit += 1;
}

fn step(
    runtimes: &mut [Runtime],
    records: &mut Vec<Record>,
    random: &mut Random,
    stats: &mut Coverage,
) {
    let index = random.below(runtimes.len());
    match random.below(100) {
        0..=19 => runtimes[index].current().enter(),
        20..=31 => runtimes[index].any(random).exit(random),
        32..=43 => {
            let record = runtimes[index].any(random).record();
            records.push(record);
        }
        44..=65 => {
            if !records.is_empty() {
                let record = records[random.below(records.len())];
                deliver_somewhere(runtimes, &record, random, stats);
            }
        }
        66..=73 => runtimes[index].any(random).invalidate(random),
        74..=80 => runtimes[index].any(random).finish_drain(),
        81..=87 => runtimes[index].any(random).destroy(),
        88..=93 => runtimes[index].replace(random, stats),
        94 => {
            let record = runtimes[index].abandon(stats);
            records.push(record);
        }
        95..=96 => records.push(foreign_record()),
        _ => exit_on_wrong_attachment(runtimes, random, stats),
    }
    for runtime in runtimes.iter_mut() {
        for attachment in &mut runtime.attachments {
            attachment.check();
        }
    }
}

fn run_sequence(random: &mut Random, stats: &mut Coverage) {
    let mut runtimes: Vec<Runtime> = (0..RUNTIMES).map(|_| Runtime::new()).collect();
    let mut records = Vec::new();
    for _ in 0..STEPS_PER_SEQUENCE {
        step(&mut runtimes, &mut records, random, stats);
    }
    for runtime in &mut runtimes {
        for attachment in &mut runtime.attachments {
            attachment.teardown();
        }
    }
    // Every record, including those for the newest epoch, is late now.
    for record in &records {
        for runtime in &mut runtimes {
            for attachment in &mut runtime.attachments {
                attachment.deliver(record, stats);
                attachment.check();
            }
        }
    }
}

#[test]
fn randomized_multi_runtime_sequences_match_reference() {
    let mut random = Random(SEED);
    let mut stats = Coverage::default();
    for _ in 0..SEQUENCES {
        run_sequence(&mut random, &mut stats);
    }
    // The schedule must actually reach every rejection class it claims to test.
    assert!(stats.accepted > 0, "{stats:?}");
    assert!(stats.epoch_mismatch > 0, "{stats:?}");
    assert!(stats.wrong_runtime > 0, "{stats:?}");
    assert!(stats.gate_draining > 0, "{stats:?}");
    assert!(stats.gate_invalid > 0, "{stats:?}");
    assert!(stats.gate_destroyed > 0, "{stats:?}");
    assert!(stats.abandoned > 0, "{stats:?}");
    assert!(stats.replacements > 0, "{stats:?}");
    assert!(stats.wrong_exit > 0, "{stats:?}");
}
