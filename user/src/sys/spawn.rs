//! The argv-vector spawn (`spawnv`, syscall 31) and this program's own
//! arguments and environment (syscall 9), fs F3 (issue #507).
//!
//! The kernel keeps a native program's `argv` (with `argv[0]`) and `envp` as
//! blocks of NUL-terminated strings; [`args`] and [`env`] read them once and
//! hand out `&'static str` views. [`service_args`] is the single-string view
//! the services that still parse one line use: `argv[1..]` joined by spaces.

use alloc::vec;
use alloc::vec::Vec;
use core::arch::asm;
use core::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};

use super::{Cred, SYS_ARGS, SYS_SPAWNV};

/// syscall 9 selector: the `argv` block.
const ARGS_ARGV: u64 = 0;
/// syscall 9 selector: the `envp` block.
const ARGS_ENVP: u64 = 1;

/// Which ABI a [`spawnv`] child runs under.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Personality {
    /// A native LazyOS program: it reads [`args`] and [`env`].
    Native,
    /// A static Linux (musl) program: `argv`/`envp` arrive on its start stack.
    Linux,
}

/// The identity a [`spawnv`] child starts with.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SpawnCred<'a> {
    /// The caller's own credentials.
    Inherit,
    /// Stamped with this credential before it can run an instruction
    /// (`CAP_SETUID`, never wider than the caller): the login path, a shell
    /// that owns its user's identity from the start.
    As(Cred),
    /// Stamped with the credential and the label string (`app:<name>`,
    /// `system:<name>` or `dev:<name>`, interned by the kernel; the
    /// credential's `label_id` is ignored). Only an unlabelled `CAP_SETUID`
    /// holder (or one already in that label) may, and a label never changes
    /// afterwards; the exception is a labelled IDE spawning into an approved
    /// `dev:` label (`kernel/src/ipc/devspawn.rs`).
    AsLabelled(Cred, &'a str),
}

/// Limits the kernel enforces (`process::spawnv`); a request over one fails
/// with `-ENAMETOOLONG` (the path) or `-E2BIG`.
pub const SPAWN_PATH_MAX: usize = 255;
/// Largest `argv` or `envp` block: the strings plus one NUL each.
pub const SPAWN_BLOCK_MAX: usize = 4096;
/// Most `argv` or `envp` strings.
pub const SPAWN_COUNT_MAX: usize = 64;

/// Start the program at `path` as a child of the calling task, with `argv`
/// (`argv[0]` included, conventionally the program name) and `envp`
/// (`KEY=VALUE` strings). Nothing is split: a path or an argument may contain
/// spaces. Returns the child's pid, or the negative errno.
pub fn spawnv(
    path: &str,
    argv: &[&str],
    envp: &[&str],
    personality: Personality,
    cred: SpawnCred<'_>,
) -> Result<u64, i64> {
    let argv_block = block(argv);
    let envp_block = block(envp);
    let (mode, cred_words, label) = match cred {
        SpawnCred::Inherit => (0, [0; 5], ""),
        SpawnCred::As(cred) => (1, cred.to_words(), ""),
        SpawnCred::AsLabelled(cred, label) => (2, cred.to_words(), label),
    };
    let request: [u64; 17] = [
        path.as_ptr() as u64,
        path.len() as u64,
        argv_block.as_ptr() as u64,
        argv_block.len() as u64,
        argv.len() as u64,
        envp_block.as_ptr() as u64,
        envp_block.len() as u64,
        envp.len() as u64,
        match personality {
            Personality::Native => 0,
            Personality::Linux => 1,
        },
        mode,
        cred_words[0],
        cred_words[1],
        cred_words[2],
        cred_words[3],
        cred_words[4],
        label.as_ptr() as u64,
        label.len() as u64,
    ];
    let code: u64;
    // Safety: `int 0x80` with syscall 31; the request and every buffer it
    // points at live until the call returns, and the kernel validates them.
    unsafe {
        asm!(
            "int 0x80",
            in("rax") SYS_SPAWNV,
            in("rdi") request.as_ptr() as u64,
            lateout("rax") code,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
            clobber_abi("sysv64"),
        );
    }
    let code = code as i64;
    if code < 0 {
        Err(code)
    } else {
        Ok(code as u64)
    }
}

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

/// Each string followed by a NUL.
fn block(items: &[&str]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(items.iter().map(|item| item.len() + 1).sum());
    for item in items {
        bytes.extend_from_slice(item.as_bytes());
        bytes.push(0);
    }
    bytes
}

/// syscall 9: copy block `which` into `buf`, returning its full length (or a
/// negative errno).
fn args_syscall(buf: &mut [u8], which: u64) -> i64 {
    let length: u64;
    // Safety: `int 0x80` with syscall 9; the buffer is valid for its length.
    unsafe {
        asm!(
            "int 0x80",
            in("rax") SYS_ARGS,
            in("rdi") buf.as_mut_ptr() as u64,
            in("rsi") buf.len() as u64,
            in("rdx") which,
            lateout("rax") length,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
            clobber_abi("sysv64"),
        );
    }
    length as i64
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
    let length = args_syscall(&mut [], which);
    if length <= 0 {
        return Vec::new();
    }
    let mut bytes = vec![0u8; length as usize];
    let copied = args_syscall(&mut bytes, which);
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
