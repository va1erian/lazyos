//! Where a socket receive puts its bytes: one user buffer (`recv`, `read`)
//! or every segment of a `recvmsg` iovec, filled in order. The receive paths
//! stage a message in a kernel buffer and hand it to [`Scatter::copy_out`],
//! so a datagram is read once, whole, against the combined capacity.

use alloc::vec::Vec;

use crate::user_ptr;

use super::errno::{err, EFAULT};

/// A list of user `(base, len)` segments, empty ones dropped.
pub(super) struct Scatter {
    segs: Vec<(u64, u64)>,
}

impl Scatter {
    /// One buffer.
    pub(super) fn one(base: u64, len: u64) -> Scatter {
        Scatter::new([(base, len)])
    }

    /// Segments in the order they are filled.
    pub(super) fn new(segs: impl IntoIterator<Item = (u64, u64)>) -> Scatter {
        Scatter {
            segs: segs.into_iter().filter(|&(_, len)| len != 0).collect(),
        }
    }

    /// The combined capacity.
    pub(super) fn len(&self) -> u64 {
        self.segs
            .iter()
            .fold(0u64, |total, &(_, len)| total.saturating_add(len))
    }

    /// What is left after the first `skip` bytes are filled.
    pub(super) fn after(&self, mut skip: u64) -> Scatter {
        let mut segs = Vec::new();
        for &(base, len) in &self.segs {
            if skip >= len {
                skip -= len;
                continue;
            }
            segs.push((base + skip, len - skip));
            skip = 0;
        }
        Scatter { segs }
    }

    /// Copy `bytes` (at most [`Scatter::len`]) into the segments in order.
    pub(super) fn copy_out(&self, mut bytes: &[u8]) -> Result<(), u64> {
        for &(base, len) in &self.segs {
            if bytes.is_empty() {
                break;
            }
            let take = bytes.len().min(usize::try_from(len).unwrap_or(usize::MAX));
            user_ptr::try_copy_to(base, &bytes[..take]).map_err(|_| err(EFAULT))?;
            bytes = &bytes[take..];
        }
        if bytes.is_empty() {
            Ok(())
        } else {
            Err(err(EFAULT))
        }
    }
}

/// A receive's outcome: the byte count (or `-errno`) and whether a message
/// was longer than the destination (its tail was discarded: `MSG_TRUNC`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Received {
    pub(super) result: u64,
    pub(super) truncated: bool,
}

impl Received {
    pub(super) fn of(result: u64) -> Received {
        Received {
            result,
            truncated: false,
        }
    }
}
