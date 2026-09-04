// SPDX-License-Identifier: MIT OR Apache-2.0

use super::RuntimeError;
use std::cell::Cell;

pub(super) struct LocalBudget {
    used: Cell<usize>,
    limit: usize,
}

pub(super) struct Reservation<'a> {
    budget: &'a LocalBudget,
    held: bool,
}

impl LocalBudget {
    pub(super) fn new(limit: usize) -> Self {
        Self {
            used: Cell::new(0),
            limit,
        }
    }

    pub(super) fn reserve(&self) -> Result<Reservation<'_>, RuntimeError> {
        let used = self.used.get();
        if used >= self.limit {
            return Err(RuntimeError::LocalRootLimitReached);
        }
        self.used.set(used + 1);
        Ok(Reservation {
            budget: self,
            held: true,
        })
    }

    pub(super) fn release(&self) {
        self.used.set(
            self.used
                .get()
                .checked_sub(1)
                .expect("balanced local admission"),
        );
    }
}

impl Reservation<'_> {
    pub(super) fn commit(mut self) {
        self.held = false;
    }
}

impl Drop for Reservation<'_> {
    fn drop(&mut self) {
        if self.held {
            self.budget.release();
        }
    }
}
