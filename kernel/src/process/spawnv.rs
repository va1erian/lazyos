//! syscall 30: `spawnv`, the argv-vector spawn (fs F3, issue #507).
//!
//! One entry point starts a native or Linux program as a child of the caller,
//! with an `argv` vector, an environment and, optionally, a credential stamp.
//! Paths and arguments may contain spaces: nothing is split or re-joined.
//! `rdi` points at a [`REQ_WORDS`]-word request block (little-endian `u64`s):
//!
//! | word | field |
//! |------|-------|
//! | 0, 1 | `path_ptr`, `path_len` (no NUL; at most [`PATH_MAX`] bytes) |
//! | 2, 3, 4 | `argv_ptr`, `argv_len`, `argc`: `argc` NUL-terminated strings, `argv[0]` included |
//! | 5, 6, 7 | `envp_ptr`, `envp_len`, `envc`: `envc` NUL-terminated `KEY=VALUE` strings |
//! | 8 | `personality`: [`personality::NATIVE`] or [`personality::LINUX`] |
//! | 9 | `cred`: [`cred_mode::INHERIT`], [`cred_mode::AS`] or [`cred_mode::AS_LABELLED`] |
//! | 10..=14 | the credential block (`uid`, `gid`, `caps`, `label_id`, `session`), read for `AS`/`AS_LABELLED` |
//! | 15, 16 | `label_ptr`, `label_len`, read for `AS_LABELLED` |
//!
//! Everything is validated before anything is allocated or loaded: an
//! overlong path is `-ENAMETOOLONG`; an overlong block or count `-E2BIG`; a
//! zero `argc`, a count that does not match the NULs, a block not ending in
//! NUL, an `envp` entry without `=`, a NUL in the path, non-UTF-8 text a
//! native program could not read, or an unknown selector `-EINVAL`; memory
//! that is not readable user memory `-EFAULT`. The credential modes are the
//! credential gate's `SPAWN`/`SPAWN_LABELLED` (syscall 10) with the same
//! checks and errors. Returns the child's pid (its slot).
//!
//! A native child reads its blocks through syscall 9 ([`super::argstore`]);
//! a Linux child receives them unchanged on its start stack.

use alloc::string::String;
use alloc::vec::Vec;

use super::argstore;
use super::creds::{approve_labelled, syscall_error, transition_error, EFAULT, EINVAL, ENOENT};
use super::creds::{EACCES, ENOMEM};
use super::spawn::intern_service_name;
use crate::fs;
use crate::ipc::credentials::{self, Cred, LabelStamp};
use crate::task;
use crate::user_ptr;

/// `E2BIG`: an argument block or count over its limit.
pub const E2BIG: i64 = 7;
/// `ENAMETOOLONG`: a path over [`PATH_MAX`].
pub const ENAMETOOLONG: i64 = 36;

/// Longest path, in bytes.
pub const PATH_MAX: usize = 255;
/// Largest `argv` block, in bytes (NULs included).
pub const ARGV_MAX: usize = 4096;
/// Largest `envp` block, in bytes (NULs included).
pub const ENVP_MAX: usize = 4096;
/// Most `argv` strings.
pub const ARGC_MAX: u64 = 64;
/// Most `envp` strings.
pub const ENVC_MAX: u64 = 64;
/// Words in the request block.
pub const REQ_WORDS: usize = 17;

/// The `personality` word.
pub mod personality {
    /// A native LazyOS program (arguments through syscall 9).
    pub const NATIVE: u64 = 0;
    /// A Linux-ABI (static musl) program (arguments on the start stack).
    pub const LINUX: u64 = 1;
}

/// The `cred` word.
pub mod cred_mode {
    /// The child inherits the caller's credentials.
    pub const INHERIT: u64 = 0;
    /// The child is stamped with the request's credential block.
    pub const AS: u64 = 1;
    /// The child is stamped with the credential block and the label string.
    pub const AS_LABELLED: u64 = 2;
}

/// The credential half of a request.
enum CredReq {
    Inherit,
    As(Cred),
    AsLabelled(Cred, String),
}

/// A validated request, copied into the kernel.
struct Request {
    path: String,
    linux: bool,
    argv: Vec<u8>,
    envp: Vec<u8>,
    cred: CredReq,
}

/// syscall 30: see the module docs.
pub(super) fn sys_spawnv(req_ptr: u64) -> u64 {
    match spawnv(req_ptr) {
        Ok(slot) => slot as u64,
        Err(code) => code,
    }
}

/// The body of [`sys_spawnv`]; the error is the syscall return value.
fn spawnv(req_ptr: u64) -> Result<usize, u64> {
    let words = read_words(req_ptr)?;
    let request = read_request(&words)?;
    let stamp = approve(request.cred)?;
    let elf = load(&request.path, request.linux)?;
    start(
        &request.path,
        request.linux,
        &elf,
        request.argv,
        request.envp,
        stamp,
    )
}

/// Copy the request block out of user memory.
fn read_words(req_ptr: u64) -> Result<[u64; REQ_WORDS], u64> {
    if req_ptr == 0 {
        return Err(syscall_error(EFAULT));
    }
    let bytes = user_ptr::try_bytes(req_ptr, REQ_WORDS * 8).map_err(|_| syscall_error(EFAULT))?;
    let mut words = [0u64; REQ_WORDS];
    for (word, chunk) in words.iter_mut().zip(bytes.as_chunks::<8>().0) {
        *word = u64::from_le_bytes(*chunk);
    }
    Ok(words)
}

/// Validate the request words: every length and selector first (no copy, no
/// allocation), then the strings themselves.
fn read_request(words: &[u64; REQ_WORDS]) -> Result<Request, u64> {
    let [path_ptr, path_len, argv_ptr, argv_len, argc, envp_ptr, envp_len, envc, personality, cred, ..] =
        *words;
    let fail = |code| Err(syscall_error(code));
    let linux = match personality {
        personality::NATIVE => false,
        personality::LINUX => true,
        _ => return fail(EINVAL),
    };
    if cred > cred_mode::AS_LABELLED {
        return fail(EINVAL);
    }
    if path_len == 0 {
        return fail(ENOENT);
    }
    if path_len > PATH_MAX as u64 {
        return fail(ENAMETOOLONG);
    }
    if argc > ARGC_MAX
        || envc > ENVC_MAX
        || argv_len > ARGV_MAX as u64
        || envp_len > ENVP_MAX as u64
    {
        return fail(E2BIG);
    }
    if argc == 0 || argv_len == 0 || (envc == 0) != (envp_len == 0) {
        return fail(EINVAL);
    }
    let path = copy_in(path_ptr, path_len)?;
    let argv = copy_in(argv_ptr, argv_len)?;
    let envp = if envc == 0 {
        Vec::new()
    } else {
        copy_in(envp_ptr, envp_len)?
    };
    // A native program reads its blocks as `&str`; Linux takes any bytes.
    let text_ok = |block: &[u8]| linux || core::str::from_utf8(block).is_ok();
    if !block_matches(&argv, argc)
        || !block_matches(&envp, envc)
        || !env_entries_ok(&envp)
        || !text_ok(&argv)
        || !text_ok(&envp)
    {
        return fail(EINVAL);
    }
    let Ok(path) = String::from_utf8(path) else {
        return fail(EINVAL);
    };
    if path.contains('\0') {
        return fail(EINVAL);
    }
    let cred = read_cred_req(cred, words)?;
    Ok(Request {
        path,
        linux,
        argv,
        envp,
        cred,
    })
}

/// Copy `len` (already bounded) bytes of user memory.
fn copy_in(ptr: u64, len: u64) -> Result<Vec<u8>, u64> {
    user_ptr::try_bytes(ptr, len as usize)
        .map(<[u8]>::to_vec)
        .map_err(|_| syscall_error(EFAULT))
}

/// Whether `block` is exactly `count` NUL-terminated strings.
fn block_matches(block: &[u8], count: u64) -> bool {
    let nuls = block.iter().filter(|byte| **byte == 0).count() as u64;
    nuls == count && block.last().is_none_or(|last| *last == 0)
}

/// Whether every `envp` entry is `KEY=VALUE` with a non-empty key.
fn env_entries_ok(envp: &[u8]) -> bool {
    entries(envp).all(|entry| {
        entry
            .iter()
            .position(|byte| *byte == b'=')
            .is_some_and(|eq| eq > 0)
    })
}

/// The strings of a NUL-terminated block, without their NULs.
fn entries(block: &[u8]) -> impl Iterator<Item = &[u8]> {
    block
        .split_inclusive(|byte| *byte == 0)
        .map(|entry| &entry[..entry.len() - 1])
}

/// Read the credential half of the request.
fn read_cred_req(mode: u64, words: &[u64; REQ_WORDS]) -> Result<CredReq, u64> {
    let cred = Cred::from_words([words[10], words[11], words[12], words[13], words[14]]);
    match mode {
        cred_mode::INHERIT => Ok(CredReq::Inherit),
        cred_mode::AS => Ok(CredReq::As(cred)),
        _ => {
            let (label_ptr, label_len) = (words[15], words[16]);
            if label_len == 0 || label_len > crate::ipc::labels::MAX_LABEL_BYTES as u64 {
                return Err(syscall_error(EINVAL));
            }
            let label = String::from_utf8(copy_in(label_ptr, label_len)?)
                .map_err(|_| syscall_error(EINVAL))?;
            Ok(CredReq::AsLabelled(cred, label))
        }
    }
}

/// The stamp an approved credential request applies to the child: the
/// credential and whether its label is assigned (labelled) or kept.
type Stamp = Option<(Cred, bool)>;

/// Run the credential gate's checks for the request, before a task exists.
fn approve(cred: CredReq) -> Result<Stamp, u64> {
    match cred {
        CredReq::Inherit => Ok(None),
        CredReq::As(cred) => {
            credentials::check(task::current(), cred).map_err(transition_error)?;
            Ok(Some((cred, false)))
        }
        CredReq::AsLabelled(cred, label) => Ok(Some((approve_labelled(cred, &label)?, true))),
    }
}

/// Read the program. Mirrors `spawn::spawn_program`: the `noexec` mount check
/// first, then a Linux program may be a BusyBox applet alias, a native one is
/// always a real file. (Keep the two in step until syscall 6 is retired.)
fn load(path: &str, linux: bool) -> Result<Vec<u8>, u64> {
    if fs::mount_flags(path).noexec {
        return Err(syscall_error(EACCES));
    }
    let elf = if linux {
        super::linux::load_executable(path).or_else(|| fs::read(path))
    } else {
        fs::read(path)
    };
    elf.ok_or(syscall_error(ENOENT))
}

/// Create the child, stamp it and record its blocks. Interrupts are off in
/// the syscall gate, so the child cannot run before the stamp and the blocks
/// are in place.
fn start(
    path: &str,
    linux: bool,
    elf: &[u8],
    argv: Vec<u8>,
    envp: Vec<u8>,
    stamp: Stamp,
) -> Result<usize, u64> {
    let name = intern_service_name(path);
    let started = if linux {
        let argv: Vec<&[u8]> = entries(&argv).collect();
        let envp: Vec<&[u8]> = entries(&envp).collect();
        task::spawn_linux_child_env(name, elf, &argv, &envp)
    } else {
        task::spawn_child(name, elf)
    };
    let slot = started.map_err(|_| syscall_error(ENOMEM))?;
    if let Some((cred, assign)) = stamp {
        // `approve` ran before the spawn, so this cannot fail; if it ever did,
        // the child would keep the identity it inherited from the caller (no
        // more privileged than the caller), as in `spawn_program`.
        let label = if assign {
            LabelStamp::Assign
        } else {
            LabelStamp::Keep {
                current: credentials::of(slot).label_id,
            }
        };
        let _ = credentials::transition_with(task::current(), slot, cred, label);
    }
    if linux {
        // The start stack holds them; nothing for syscall 9 to keep.
        argstore::forget(slot);
    } else {
        argstore::set(slot, argv, envp);
    }
    Ok(slot)
}
