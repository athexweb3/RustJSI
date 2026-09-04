// SPDX-License-Identifier: MIT OR Apache-2.0

use std::panic::{UnwindSafe, catch_unwind};

// Payload destruction is user code too. Reclaim an ordinary payload, but do
// not recursively drop a second payload if the first payload's Drop panics.
// That exceptional path intentionally leaks the second payload. Abort-mode
// panics, aborting hooks, and double panics during stack unwinding are fatal.
pub(super) fn contain_unwind<R>(operation: impl FnOnce() -> R + UnwindSafe) -> Result<R, ()> {
    match catch_unwind(operation) {
        Ok(value) => Ok(value),
        Err(payload) => {
            if let Err(secondary) = catch_unwind(std::panic::AssertUnwindSafe(|| drop(payload))) {
                std::mem::forget(secondary);
            }
            Err(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Payload {
        drops: Arc<AtomicUsize>,
        panic: bool,
    }

    impl Drop for Payload {
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::Relaxed);
            if self.panic {
                std::panic::panic_any(Self {
                    drops: Arc::clone(&self.drops),
                    panic: false,
                });
            }
        }
    }

    #[test]
    fn ordinary_payload_is_reclaimed() {
        let drops = Arc::new(AtomicUsize::new(0));
        let payload = Payload {
            drops: Arc::clone(&drops),
            panic: false,
        };
        assert!(contain_unwind(|| std::panic::panic_any(payload)).is_err());
        assert_eq!(drops.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn panicking_payload_cannot_escape_boundary() {
        let drops = Arc::new(AtomicUsize::new(0));
        let payload = Payload {
            drops: Arc::clone(&drops),
            panic: true,
        };
        assert!(contain_unwind(|| std::panic::panic_any(payload)).is_err());
        assert_eq!(drops.load(Ordering::Relaxed), 1);
        // The secondary payload is deliberately not destroyed or retried.
        assert_eq!(Arc::strong_count(&drops), 2);
    }
}
