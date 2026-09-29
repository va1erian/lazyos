//! The error a refused quota charge carries (split out of `quota.rs`, issue
//! #194).

use alloc::string::String;
use core::fmt;

use super::Resource;

/// A refused charge: which resource, whose, and how full it was. The friendly
/// [`QuotaError::message`] carries the same numbers for userspace.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct QuotaError {
    /// User the charge was for.
    pub uid: u32,
    /// Resource that ran out.
    pub resource: Resource,
    /// Live usage before the refused charge.
    pub usage: u64,
    /// Configured limit.
    pub limit: u64,
}

impl QuotaError {
    /// The friendly, user-facing explanation: resource name plus current usage
    /// and limit, matching the convention in `docs/security-model.md` (never a
    /// bare `EPERM`/`ENOSPC`).
    pub fn message(&self) -> String {
        alloc::format!(
            "uid {} is over its {} quota: {} of {} {} in use",
            self.uid,
            self.resource.name(),
            self.usage,
            self.limit,
            self.resource.unit()
        )
    }
}

impl fmt::Debug for QuotaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "QuotaError({}, {:?}, {}/{})",
            self.uid, self.resource, self.usage, self.limit
        )
    }
}

impl fmt::Display for QuotaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message())
    }
}
