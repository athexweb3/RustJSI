// SPDX-License-Identifier: MIT OR Apache-2.0

//! Differential checks against the independent lifecycle event model.

use rustjsi_host::{EntryGate, HostState};
use rustjsi_testkit::{Epoch, LifecycleModel, RuntimeId, RuntimeState};
use std::num::NonZeroU32;

#[test]
fn gate_matches_independent_model_over_one_hundred_thousand_operations() {
    let mut random = 0x05ee_da11_u64;
    for cycle in 1..=1_000 {
        let gate = EntryGate::new(NonZeroU32::new(100).unwrap());
        let mut model = LifecycleModel::new(RuntimeId::new(cycle), Epoch::new(1));
        let mut entries = Vec::new();
        let mut cleanup = None;
        for _ in 0..100 {
            random ^= random << 13;
            random ^= random >> 7;
            random ^= random << 17;
            match random % 10 {
                0..=3 => {
                    let actual = gate.try_enter();
                    let expected = model.enter();
                    assert_eq!(actual.is_ok(), expected.is_ok());
                    if let (Ok(actual), Ok(expected)) = (actual, expected) {
                        entries.push((actual, expected));
                    }
                }
                4 => {
                    if let Some((actual, expected)) = entries.pop() {
                        drop(actual);
                        model.exit(expected).unwrap();
                    }
                }
                5 => {
                    gate.request_drain();
                    model.request_invalidate();
                }
                6 if cleanup.is_some() => assert!(gate.finish_drain().is_err()),
                6 => assert_eq!(gate.finish_drain().is_ok(), model.finish_drain().is_ok()),
                7 => assert_eq!(gate.mark_destroyed().is_ok(), model.destroy().is_ok()),
                8 => {
                    let expected = model.state() == RuntimeState::Draining
                        && model.active_entries() == 0
                        && cleanup.is_none();
                    let actual = gate.try_begin_cleanup();
                    assert_eq!(actual.is_ok(), expected);
                    if let Ok(guard) = actual {
                        cleanup = Some(guard);
                    }
                }
                _ => drop(cleanup.take()),
            }
            assert_eq!(gate.active_entries(), model.active_entries());
            let expected = match model.state() {
                RuntimeState::Active => HostState::Active,
                RuntimeState::Draining => HostState::Draining,
                RuntimeState::Invalid => HostState::Invalid,
                RuntimeState::Destroyed => HostState::Destroyed,
            };
            assert_eq!(gate.state(), expected);
            assert_eq!(
                gate.is_drain_ready(),
                model.state() == RuntimeState::Draining
                    && model.active_entries() == 0
                    && cleanup.is_none()
            );
            assert_eq!(gate.cleanup_in_progress(), cleanup.is_some());
        }
        while let Some((actual, expected)) = entries.pop() {
            drop(actual);
            model.exit(expected).unwrap();
        }
        drop(cleanup);
        gate.request_drain();
        model.request_invalidate();
        if gate.state() != HostState::Destroyed {
            gate.finish_drain().unwrap();
            model.finish_drain().unwrap();
            gate.mark_destroyed().unwrap();
            model.destroy().unwrap();
        }
        assert_eq!(gate.state(), HostState::Destroyed);
        assert_eq!(gate.active_entries(), 0);
    }
}
