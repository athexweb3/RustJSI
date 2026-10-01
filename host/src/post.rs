// SPDX-License-Identifier: MIT OR Apache-2.0

use std::error::Error;

use crate::AttachmentId;

/// Source-linked capability for posting one future attachment drain.
///
/// A poster receives identity only. It does not receive backend access, queue
/// payloads, or a callback that could retain a borrowed host entry.
pub trait DrainPoster {
    /// Host-specific post failure.
    type Error: Error;

    /// Requests one future legal drain for `attachment`.
    ///
    /// Success means only that the host accepted the post. The eventual drain
    /// must still validate lifecycle, attachment epoch, and legal entry.
    ///
    /// # Errors
    ///
    /// Returns the host-specific failure when it cannot accept the post.
    fn post_drain(&self, attachment: AttachmentId) -> Result<(), Self::Error>;
}
