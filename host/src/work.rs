// SPDX-License-Identifier: MIT OR Apache-2.0

use std::error::Error;
use std::fmt;

use rustjsi_backend::BackendFamily;

use crate::{AttachmentId, Host};

/// Owned native work captured for one engine attachment.
///
/// A producer creates this record before placing it into a host-owned queue.
/// The record's payload can enter backend work only through [`Self::dispatch`],
/// which compares the captured attachment with the host's current attachment
/// and then borrows the backend through [`Host::with_backend`].
#[derive(Debug)]
pub struct ScheduledWork<T> {
    attachment: AttachmentId,
    payload: T,
}

/// Failure to dispatch [`ScheduledWork`] through a host.
///
/// Normal rejection paths retain the original work record so a queue owner can
/// apply its terminal policy without a hidden payload drop. A contract
/// violation reports whether the work remained available after the host's
/// unexpected result.
#[derive(Debug)]
#[must_use]
pub enum WorkDispatchError<T, E> {
    /// The record was captured for a different runtime or attachment epoch.
    AttachmentMismatch {
        /// The rejected record.
        work: ScheduledWork<T>,
        /// The attachment currently reported by the host.
        active_attachment: AttachmentId,
    },
    /// The current host rejected legal entry without running the operation.
    Entry {
        /// The rejected record.
        work: ScheduledWork<T>,
        /// The host's entry failure.
        error: E,
    },
    /// The host returned a result inconsistent with [`Host::with_backend`].
    ///
    /// This is defensive diagnostics for a broken host implementation. `work`
    /// is present when the host did not invoke the operation and absent when it
    /// invoked the operation before reporting an error.
    HostContractViolation {
        /// The attachment reported before the attempted host entry.
        active_attachment: AttachmentId,
        /// Work still owned by the caller, when available.
        work: Option<ScheduledWork<T>>,
    },
}

enum WorkInvocation<R> {
    Ran(R),
    MissingPayload,
}

impl<T> ScheduledWork<T> {
    /// Captures owned work for one engine attachment.
    #[must_use]
    pub fn new(attachment: AttachmentId, payload: T) -> Self {
        Self {
            attachment,
            payload,
        }
    }

    /// Returns the attachment captured when this work was created.
    #[must_use]
    pub const fn attachment_id(&self) -> AttachmentId {
        self.attachment
    }

    /// Releases the owned payload without granting backend access.
    ///
    /// Queue owners use this only for terminal rejection, diagnostics, or a
    /// separately-defined retry policy. It does not validate an attachment or
    /// make any engine operation legal.
    #[must_use]
    pub fn into_payload(self) -> T {
        self.payload
    }

    /// Dispatches this work through one legal host entry.
    ///
    /// The captured attachment must match the host's current attachment before
    /// the operation can run. The host then performs its authoritative
    /// lifecycle and thread-entry check. If either check rejects the record,
    /// the error retains it.
    ///
    /// # Errors
    ///
    /// Returns [`WorkDispatchError::AttachmentMismatch`] before host entry for
    /// a stale or foreign attachment, or [`WorkDispatchError::Entry`] when the
    /// host rejects entry. Both normal errors retain the original work record.
    pub fn dispatch<H, R>(
        self,
        host: &mut H,
        operation: impl for<'entry> FnOnce(&mut <H::Family as BackendFamily>::Backend<'entry>, T) -> R,
    ) -> Result<R, WorkDispatchError<T, H::Error>>
    where
        H: Host,
    {
        let active_attachment = host.attachment_id();
        if self.attachment != active_attachment {
            return Err(WorkDispatchError::AttachmentMismatch {
                work: self,
                active_attachment,
            });
        }

        let mut work = Some(self);
        let result = host.with_backend(|backend| match work.take() {
            Some(work) => WorkInvocation::Ran(operation(backend, work.payload)),
            None => WorkInvocation::MissingPayload,
        });

        match result {
            Ok(WorkInvocation::Ran(value)) => Ok(value),
            Ok(WorkInvocation::MissingPayload) => Err(WorkDispatchError::HostContractViolation {
                active_attachment,
                work,
            }),
            Err(error) => match work {
                Some(work) => Err(WorkDispatchError::Entry { work, error }),
                None => Err(WorkDispatchError::HostContractViolation {
                    active_attachment,
                    work: None,
                }),
            },
        }
    }
}

impl<T, E> WorkDispatchError<T, E> {
    /// Returns the rejected record when the host did not consume it.
    #[must_use]
    pub fn into_work(self) -> Option<ScheduledWork<T>> {
        match self {
            Self::AttachmentMismatch { work, .. } | Self::Entry { work, .. } => Some(work),
            Self::HostContractViolation { work, .. } => work,
        }
    }
}

impl<T, E> fmt::Display for WorkDispatchError<T, E>
where
    E: fmt::Display,
{
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AttachmentMismatch {
                active_attachment, ..
            } => write!(
                formatter,
                "scheduled work targets a different attachment than {}:{}",
                active_attachment.runtime_id().get(),
                active_attachment.epoch().get()
            ),
            Self::Entry { error, .. } => write!(formatter, "host rejected scheduled work: {error}"),
            Self::HostContractViolation { .. } => {
                formatter.write_str("host violated the scheduled-work entry contract")
            }
        }
    }
}

impl<T, E> Error for WorkDispatchError<T, E>
where
    T: fmt::Debug + 'static,
    E: Error + 'static,
{
}
