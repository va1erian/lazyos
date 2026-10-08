//! This program's own arguments and environment (syscall 9, fs F3, issue
//! #507), and the inheriting spawn shorthands over `lazyos_sys::spawn`.
//!
//! The kernel keeps a native program's `argv` (with `argv[0]`) and `envp` as
//! blocks of NUL-terminated strings; [`args`] and [`env`] read them once and
//! hand out `&'static str` views. [`service_args`] is the single-string view
//! the services that still parse one line use: `argv[1..]` joined by spaces.

use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};

use lazyos_sys::process::{args_block, ARGS_ARGV, ARGS_ENVP};
use lazyos_sys::spawn::{spawnv, Personality, SpawnCred};

/// [`spawnv`] of the native program at `program`, inheriting this task's
/// credentials, with no environment: `argv` is `program` then `args`. Returns
/// the child's pid, or `None` when the program could not be started.
pub fn spawn_native(program: &str, args: &[&str]) -> Option<u64> {
    spawn_inherit(program, args, Personality::Native)
}

/// [`spawn_native`] for a static Linux-ABI (musl) program.
pub fn spawn_linux(program: &str, args: &[&str]) -> Option<u64> {
    spawn_inherit(program, args, Personality::Linux)
}

fn spawn_inherit(program: &str, args: &[&str], personality: Personality) -> Option<u64> {
    let argv: Vec<&str> = core::iter::once(program)
        .chain(args.iter().copied())
        .collect();
    spawnv(program, &argv, &[], personality, SpawnCred::Inherit).ok()
}

/// A block read once and kept for the life of the program (it never changes).
struct Cached {
    ptr: AtomicPtr<u8>,
    len: AtomicUsize,
}

static ARGV: Cached = Cached::new();
static ENVP: Cached = Cached::new();

impl Cached {
    const fn new() -> Cached {
        Cached {
            ptr: AtomicPtr::new(core::ptr::null_mut()),
            len: AtomicUsize::new(0),
        }
    }

    /// The block, fetched with selector `which` on first use. Native programs
    /// are single-threaded; a racing second fetch would only leak one copy.
    fn get(&self, which: u64) -> &'static [u8] {
        let ptr = self.ptr.load(Ordering::Acquire);
        if !ptr.is_null() {
            // Safety: `ptr`/`len` describe a leaked, never-freed allocation
            // published below (`len` is stored before `ptr`).
            return unsafe { core::slice::from_raw_parts(ptr, self.len.load(Ordering::Relaxed)) };
        }
        let leaked: &'static mut [u8] = fetch(which).leak();
        self.len.store(leaked.len(), Ordering::Relaxed);
        // An empty block still needs a non-null marker; a dangling pointer is
        // valid for a zero-length slice.
        let ptr = if leaked.is_empty() {
            core::ptr::NonNull::dangling().as_ptr()
        } else {
            leaked.as_mut_ptr()
        };
        self.ptr.store(ptr, Ordering::Release);
        leaked
    }
}

/// Copy block `which` out of the kernel.
fn fetch(which: u64) -> Vec<u8> {
    let length = args_block(&mut [], which);
    if length <= 0 {
        return Vec::new();
    }
    let mut bytes = vec![0u8; length as usize];
    let copied = args_block(&mut bytes, which);
    bytes.truncate(copied.clamp(0, length) as usize);
    bytes
}

/// The strings of a NUL-terminated block; a string that is not UTF-8 (only a
/// kernel boot spawn could produce one) reads as empty.
#[derive(Clone, Debug)]
pub struct Strings {
    rest: &'static [u8],
}

impl Iterator for Strings {
    type Item = &'static str;

    fn next(&mut self) -> Option<&'static str> {
        if self.rest.is_empty() {
            return None;
        }
        let end = self
            .rest
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(self.rest.len());
        let item = &self.rest[..end];
        self.rest = self.rest.get(end + 1..).unwrap_or(&[]);
        Some(core::str::from_utf8(item).unwrap_or(""))
    }
}

/// This program's `argv`, `argv[0]` first. Empty for a program the kernel
/// started without arguments.
pub fn args() -> Strings {
    Strings {
        rest: ARGV.get(ARGS_ARGV),
    }
}

/// This program's environment as `(key, value)` pairs.
pub fn env() -> impl Iterator<Item = (&'static str, &'static str)> {
    Strings {
        rest: ENVP.get(ARGS_ENVP),
    }
    .filter_map(|entry| entry.split_once('='))
}

/// The value of environment variable `key`, if set.
pub fn getenv(key: &str) -> Option<&'static str> {
    env().find(|(name, _)| *name == key).map(|(_, value)| value)
}

/// Copy this program's arguments as one string (`argv[1..]` joined by single
/// spaces, the form the manifest's argument string had) into `buf`; returns
/// its full length. A zero-length `buf` reports the length without copying.
pub fn service_args(buf: &mut [u8]) -> usize {
    let mut length = 0;
    for (index, arg) in args().skip(1).enumerate() {
        if index > 0 {
            length += put(buf, length, b" ");
        }
        length += put(buf, length, arg.as_bytes());
    }
    length
}

/// Copy as much of `bytes` as fits into `buf` at `at`; returns `bytes.len()`.
fn put(buf: &mut [u8], at: usize, bytes: &[u8]) -> usize {
    if let Some(room) = buf.get_mut(at..) {
        let count = room.len().min(bytes.len());
        room[..count].copy_from_slice(&bytes[..count]);
    }
    bytes.len()
}
