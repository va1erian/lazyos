//! Per-task argument and environment blocks (fs F3, issue #507).
//!
//! A native program has no start stack, so the kernel keeps what its spawner
//! gave it here and syscall 9 copies it out: `argv` as one block of
//! NUL-terminated strings (`argv[0]` included), `envp` as the same shape of
//! `KEY=VALUE` strings. `spawnv` stores the blocks exactly as the caller sent
//! them; the kernel's boot spawns and the Linux `execve` of a native program
//! store their `argv` vector through [`block`], so every path feeds one
//! format and nothing is ever split or re-joined.
//!
//! Every spawn path writes the slot's entry before the child can run (the
//! syscall gate holds interrupts off), and reaping the task forgets it
//! ([`forget`]), so a reused slot never sees another program's arguments and
//! a dead task holds no heap.

use alloc::vec::Vec;
use spin::Mutex;

use super::creds::{syscall_error, EFAULT, EINVAL};
use crate::task;
use crate::user_ptr;

/// The block syscall 9 copies, selected by its third argument.
pub mod which {
    /// The `argv` block.
    pub const ARGV: u64 = 0;
    /// The `envp` block.
    pub const ENVP: u64 = 1;
}

/// One task's blocks.
struct Blocks {
    argv: Vec<u8>,
    envp: Vec<u8>,
}

/// The blocks, keyed by task slot.
static BLOCKS: Mutex<[Option<Blocks>; task::MAX_TASKS]> =
    Mutex::new([const { None }; task::MAX_TASKS]);

/// Record `slot`'s `argv` and `envp` blocks, replacing any earlier entry.
pub(super) fn set(slot: usize, argv: Vec<u8>, envp: Vec<u8>) {
    let previous = BLOCKS
        .lock()
        .get_mut(slot)
        .and_then(|entry| entry.replace(Blocks { argv, envp }));
    // Freed outside the table lock.
    drop(previous);
}

/// Forget `slot`'s blocks: the task was reaped (or the harness freed it).
pub(crate) fn forget(slot: usize) {
    let previous = BLOCKS.lock().get_mut(slot).and_then(Option::take);
    drop(previous);
}

/// The block of `items`: each followed by its NUL. The caller guarantees no
/// item contains a NUL (each is one C string or one validated argument).
pub(super) fn block<A: AsRef<[u8]>>(items: &[A]) -> Vec<u8> {
    let mut block = Vec::with_capacity(items.iter().map(|item| item.as_ref().len() + 1).sum());
    for item in items {
        block.extend_from_slice(item.as_ref());
        block.push(0);
    }
    block
}

/// syscall 9: copy the calling task's `argv` ([`which::ARGV`]) or `envp`
/// ([`which::ENVP`]) block into `buf`.
///
/// Returns the block's full length; at most `buf_len` bytes are copied, so a
/// caller sizes its buffer from a first zero-length call. A task started with
/// no arguments has an empty block. An unknown selector is `-EINVAL`, an
/// unwritable buffer `-EFAULT`.
pub(super) fn sys_args(buf_ptr: u64, buf_len: u64, selector: u64) -> u64 {
    if selector != which::ARGV && selector != which::ENVP {
        return syscall_error(EINVAL);
    }
    let block = {
        let blocks = BLOCKS.lock();
        match blocks.get(task::current()).and_then(Option::as_ref) {
            Some(entry) if selector == which::ARGV => entry.argv.clone(),
            Some(entry) => entry.envp.clone(),
            None => Vec::new(),
        }
    };
    let count = block
        .len()
        .min(usize::try_from(buf_len).unwrap_or(usize::MAX));
    if count > 0 && user_ptr::try_copy_to(buf_ptr, &block[..count]).is_err() {
        return syscall_error(EFAULT);
    }
    block.len() as u64
}

/// How many slots hold blocks (the leak checks of the spawn suite).
#[cfg(lazyos_tests)]
pub fn live_count() -> usize {
    BLOCKS.lock().iter().filter(|entry| entry.is_some()).count()
}
