//! syscall 31: `spawnv`, the argv-vector spawn (fs F3, issue #507).
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
//! | 17, 18, 19 | only with [`personality::STDIO`]: the caller's descriptors that become the child's 0, 1 and 2 ([`STDIO_TERMINAL`] keeps the terminal) |
//!
//! Without [`personality::STDIO`] a child starts on the terminal, as always.
//! With it, the block is [`REQ_WORDS_STDIO`] words and the child gets exactly
//! those three descriptors of the caller (shared like `dup`), nothing else: an
//! IDE keeps the pipes of a program it runs under a `dev:` label (issue
//! #529). A descriptor that is out of range or closed is `-EBADF`.
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
use super::creds::ENOMEM;
use super::creds::{approve_labelled, syscall_error, transition_error, EFAULT, EINVAL, ENOENT};
use super::spawn::{check_exec, intern_service_name};
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
/// Words in a request block with [`personality::STDIO`].
pub const REQ_WORDS_STDIO: usize = 20;
/// A stdio word that leaves the child's descriptor on the terminal.
pub const STDIO_TERMINAL: u64 = u64::MAX;
/// `EBADF`: a stdio word naming no open descriptor of the caller.
pub const EBADF: i64 = 9;

/// The `personality` word.
pub mod personality {
    /// A native LazyOS program (arguments through syscall 9).
    pub const NATIVE: u64 = 0;
    /// A Linux-ABI (static musl) program (arguments on the start stack).
    pub const LINUX: u64 = 1;
    /// Flag: the block carries the child's standard streams (words 17-19).
    pub const STDIO: u64 = 1 << 8;
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
    /// The caller's descriptors for the child's 0, 1 and 2 (`None`: the
    /// terminal), when the request asked for them.
    stdio: Option<[Option<usize>; 3]>,
}

/// syscall 31: see the module docs.
pub(super) fn sys_spawnv(req_ptr: u64) -> u64 {
    match spawnv(req_ptr) {
        Ok(slot) => slot as u64,
        Err(code) => code,
    }
}

/// The body of [`sys_spawnv`]; the error is the syscall return value.
fn spawnv(req_ptr: u64) -> Result<usize, u64> {
    let words = read_words(req_ptr)?;
    let mut request = read_request(&words)?;
    let stamp = approve(core::mem::replace(&mut request.cred, CredReq::Inherit))?;
    let elf = load(&request.path, request.linux)?;
    start(&request, &elf, stamp)
}

/// Copy the request block out of user memory: [`REQ_WORDS`] words, or
/// [`REQ_WORDS_STDIO`] when the personality word asks for stdio.
fn read_words(req_ptr: u64) -> Result<[u64; REQ_WORDS_STDIO], u64> {
    if req_ptr == 0 {
        return Err(syscall_error(EFAULT));
    }
    let mut words = [STDIO_TERMINAL; REQ_WORDS_STDIO];
    read_into(req_ptr, &mut words[..REQ_WORDS])?;
    if words[8] & personality::STDIO != 0 {
        read_into(req_ptr, &mut words)?;
    }
    Ok(words)
}

/// Fill `words` from the user block at `ptr`.
fn read_into(ptr: u64, words: &mut [u64]) -> Result<(), u64> {
    let bytes = user_ptr::try_bytes(ptr, words.len() * 8).map_err(|_| syscall_error(EFAULT))?;
    for (word, chunk) in words.iter_mut().zip(bytes.as_chunks::<8>().0) {
        *word = u64::from_le_bytes(*chunk);
    }
    Ok(())
}

/// The stdio words of a request: each a caller descriptor that is open, or
/// [`STDIO_TERMINAL`]. `-EBADF` otherwise.
fn read_stdio(words: &[u64; REQ_WORDS_STDIO]) -> Result<Option<[Option<usize>; 3]>, u64> {
    if words[8] & personality::STDIO == 0 {
        return Ok(None);
    }
    let mut map = [None; 3];
    for (slot, word) in map.iter_mut().zip(&words[REQ_WORDS..]) {
        if *word == STDIO_TERMINAL {
            continue;
        }
        let fd = usize::try_from(*word).unwrap_or(usize::MAX);
        if fd >= task::FD_COUNT || task::fd_kind(fd) == task::FdKind::Closed {
            return Err(syscall_error(EBADF));
        }
        *slot = Some(fd);
    }
    Ok(Some(map))
}

/// Validate the request words: every length and selector first (no copy, no
/// allocation), then the strings themselves.
fn read_request(words: &[u64; REQ_WORDS_STDIO]) -> Result<Request, u64> {
    let [path_ptr, path_len, argv_ptr, argv_len, argc, envp_ptr, envp_len, envc, personality, cred, ..] =
        *words;
    let fail = |code| Err(syscall_error(code));
    let linux = match personality & !personality::STDIO {
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
    let stdio = read_stdio(words)?;
    Ok(Request {
        path,
        linux,
        argv,
        envp,
        cred,
        stdio,
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
fn read_cred_req(mode: u64, words: &[u64; REQ_WORDS_STDIO]) -> Result<CredReq, u64> {
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
/// credential and its label rule (`None` for `Keep`: the label the child
/// inherited, read once it exists).
type Stamp = Option<(Cred, Option<LabelStamp>)>;

/// Run the credential gate's checks for the request, before a task exists.
fn approve(cred: CredReq) -> Result<Stamp, u64> {
    match cred {
        CredReq::Inherit => Ok(None),
        CredReq::As(cred) => {
            credentials::check(task::current(), cred).map_err(transition_error)?;
            Ok(Some((cred, None)))
        }
        CredReq::AsLabelled(cred, label) => {
            let (cred, rule) = approve_labelled(cred, &label)?;
            Ok(Some((cred, Some(rule))))
        }
    }
}

/// Read the program after the gate every spawn path shares
/// ([`check_exec`]: `noexec`, then `EXECUTE` on a regular file, root
/// included). A Linux program may then be a BusyBox applet alias, a native one
/// is always a real file.
fn load(path: &str, linux: bool) -> Result<Vec<u8>, u64> {
    // `check_exec` answers a negative errno; as `u64` it is the syscall value.
    check_exec(path, linux).map_err(|errno| errno as u64)?;
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
fn start(request: &Request, elf: &[u8], stamp: Stamp) -> Result<usize, u64> {
    let name = intern_service_name(&request.path);
    let started = if request.linux {
        let argv: Vec<&[u8]> = entries(&request.argv).collect();
        let envp: Vec<&[u8]> = entries(&request.envp).collect();
        task::spawn_linux_child_env(name, elf, &argv, &envp)
    } else {
        task::spawn_child(name, elf)
    };
    let slot = started.map_err(|_| syscall_error(ENOMEM))?;
    if let Some((cred, rule)) = stamp {
        // `approve` ran before the spawn, so this cannot fail; if it ever did,
        // the child would keep the identity it inherited from the caller (no
        // more privileged than the caller), as in `spawn_program`.
        let label = rule.unwrap_or(LabelStamp::Keep {
            current: credentials::of(slot).label_id,
        });
        let _ = credentials::transition_with(task::current(), slot, cred, label);
    }
    if let Some(map) = &request.stdio {
        // Validated in `read_stdio` and nothing ran since (interrupts are off
        // in the gate), so the descriptors are still open.
        let _ = task::give_stdio(slot, map);
    }
    if request.linux {
        // The start stack holds them; nothing for syscall 9 to keep.
        argstore::forget(slot);
    } else {
        argstore::set(slot, request.argv.clone(), request.envp.clone());
    }
    Ok(slot)
}
