//! The calling user's resource accounting (syscall 11, `kernel/src/quota/`).
//!
//! The kernel writes one `(usage, limit)` pair of `u64`s per resource, in
//! `Resource::ALL` order, and the call carries no length: the kernel decides how many
//! words it writes (`Resource::COUNT * 2`, `quota::STATS_WORDS`) and a new resource appends
//! a pair. [`stats`] therefore hands it a buffer far larger than any block the kernel
//! writes today, so a kernel with more resources than this crate knows of cannot overrun it.

use crate::{nr, value};

/// Words in the buffer the call is given: room for 32 resources, where the kernel has 10.
pub const WORDS_MAX: usize = 64;

/// `Resource::UserMemory`'s index in the wire order.
pub const USER_MEMORY: usize = 1;

/// One resource's accounting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Charge {
    pub used: u64,
    pub limit: u64,
}

/// The raw block: a `(usage, limit)` pair per resource, the tail after the kernel's last
/// resource zero.
pub fn stats() -> Result<[u64; WORDS_MAX], i64> {
    let mut words = [0u64; WORDS_MAX];
    // SAFETY: the kernel writes `quota::STATS_WORDS` (`Resource::COUNT * 2`, 20 today) words,
    // far fewer than `WORDS_MAX`.
    let code = unsafe { crate::raw::syscall1(nr::QUOTA, words.as_mut_ptr() as u64) };
    value(code)?;
    Ok(words)
}

/// What `resource` (an index in the wire order, such as [`USER_MEMORY`]) has used and may use.
pub fn charge(words: &[u64; WORDS_MAX], resource: usize) -> Option<Charge> {
    let at = resource.checked_mul(2)?;
    Some(Charge {
        used: *words.get(at)?,
        limit: *words.get(at + 1)?,
    })
}

/// The user-memory charge of the calling user: the length of every mapping its processes hold.
pub fn user_memory() -> Result<Charge, i64> {
    let words = stats()?;
    charge(&words, USER_MEMORY).ok_or(-crate::errno::EINVAL)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pair_is_usage_then_limit() {
        let mut words = [0u64; WORDS_MAX];
        words[2] = 7;
        words[3] = 9;
        assert_eq!(
            charge(&words, USER_MEMORY),
            Some(Charge { used: 7, limit: 9 })
        );
    }

    #[test]
    fn a_resource_past_the_block_is_none() {
        let words = [0u64; WORDS_MAX];
        assert!(charge(&words, WORDS_MAX / 2).is_none());
        assert!(charge(&words, usize::MAX).is_none());
    }
}
