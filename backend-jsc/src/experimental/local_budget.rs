// SPDX-License-Identifier: MIT OR Apache-2.0

use super::RuntimeError;
use std::cell::Cell;

pub(super) struct LocalBudget {
    used: Cell<usize>,
    limit: usize,
}

pub(super) struct Reservation<'a> {
    budget: &'a LocalBudget,
    held: usize,
}

impl LocalBudget {
    pub(super) fn new(limit: usize) -> Self {
        Self {
            used: Cell::new(0),
            limit,
        }
    }

    pub(super) fn reserve(&self) -> Result<Reservation<'_>, RuntimeError> {
        self.reserve_many(1)
    }

    pub(super) fn reserve_many(&self, count: usize) -> Result<Reservation<'_>, RuntimeError> {
        if count == 0 {
            return Ok(Reservation {
                budget: self,
                held: 0,
            });
        }
        let used = self.used.get();
        let Some(next) = used.checked_add(count) else {
            return Err(RuntimeError::LocalRootLimitReached);
        };
        if next > self.limit {
            return Err(RuntimeError::LocalRootLimitReached);
        }
        self.used.set(next);
        Ok(Reservation {
            budget: self,
            held: count,
        })
    }

    pub(super) fn release(&self) {
        self.release_many(1);
    }

    fn release_many(&self, count: usize) {
        self.used.set(
            self.used
                .get()
                .checked_sub(count)
                .expect("balanced local admission"),
        );
    }

    #[cfg(test)]
    pub(super) fn used(&self) -> usize {
        self.used.get()
    }
}

impl Reservation<'_> {
    pub(super) fn commit(mut self) {
        self.held = 0;
    }
}

impl Drop for Reservation<'_> {
    fn drop(&mut self) {
        if self.held != 0 {
            self.budget.release_many(self.held);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::LocalBudget;

    #[test]
    fn uncommitted_reservation_is_refunded_during_unwind() {
        let budget = LocalBudget::new(1);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _reservation = budget.reserve().unwrap();
            assert!(budget.reserve().is_err());
            panic!("operation before local publication");
        }));
        assert!(result.is_err());
        assert_eq!(budget.used.get(), 0);
        budget.reserve().unwrap().commit();
        assert!(budget.reserve().is_err());
        budget.release();
        assert_eq!(budget.used.get(), 0);
    }

    #[test]
    fn group_reservations_are_atomic_and_refunded() {
        let budget = LocalBudget::new(3);
        let reservation = budget.reserve_many(2).unwrap();
        assert_eq!(budget.used(), 2);
        assert!(budget.reserve_many(2).is_err());
        assert!(budget.reserve_many(usize::MAX).is_err());
        assert_eq!(budget.used(), 2);
        drop(reservation);
        assert_eq!(budget.used(), 0);
        let empty = budget.reserve_many(0).unwrap();
        assert_eq!(budget.used(), 0);
        drop(empty);
    }
}
