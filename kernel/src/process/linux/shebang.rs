//! `execve` of `#!` scripts (issue #491).
//!
//! A file whose first two bytes are `#!` names its interpreter on the rest of
//! the first line: `#!interp [one-arg]`. `execve` then runs `interp` with argv
//! `[interp, (one-arg), script, argv[1..]]`, which is how `chmod +x s.sh;
//! ./s.sh` works under BusyBox `sh` and how `#!/bin/rhai` makes a Rhai script a
//! program (docs/rhai-plan.md, P5).
//!
//! The rules follow Linux `binfmt_script`, tightened where Linux silently
//! truncates:
//!
//! - the line is at most [`MAX_LINE`] bytes including `#!`; a longer one is
//!   `ENOEXEC` rather than a truncated interpreter name;
//! - leading and trailing blanks are trimmed (a trailing `\r` too, so a script
//!   saved with CRLF endings still runs); everything after the first blank
//!   that follows the interpreter is its single argument, inner blanks kept;
//! - an empty line, a NUL byte, or an interpreter name that is not UTF-8 is
//!   `ENOEXEC`;
//! - an interpreter may itself be a script, at most [`MAX_DEPTH`] times, after
//!   which the exec fails with `ELOOP`;
//! - every hop passes the same checks as the script: the mount must not be
//!   `noexec` and the caller needs the execute bit, so a script cannot launder
//!   a non-executable interpreter. A missing interpreter is `ENOENT`.

use alloc::string::String;
use alloc::vec::Vec;

use crate::fs::vfs::{self, FsError, Id};

use super::cwd::{resolve_at, AT_FDCWD};
use super::errno::{err, fs_err, ELOOP, ENOENT, ENOEXEC};
use super::path::load_executable;

/// Longest `#!` line, `#!` included (Linux's `BINPRM_BUF_SIZE`).
pub const MAX_LINE: usize = 256;

/// How many interpreters may be scripts themselves before `ELOOP` (Linux's
/// `BINPRM_MAX_RECURSION`).
pub const MAX_DEPTH: usize = 4;

/// The parsed `#!` line: the interpreter and its optional single argument.
#[derive(Debug, PartialEq, Eq)]
pub struct Interp<'a> {
    pub path: &'a str,
    pub arg: Option<&'a [u8]>,
}

/// An executable ready to load: the ELF's bytes and the argv it runs with
/// (each entry NUL-terminated).
pub struct Image {
    pub elf: Vec<u8>,
    pub argv: Vec<Vec<u8>>,
}

fn is_blank(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\r')
}

fn trim(mut bytes: &[u8]) -> &[u8] {
    while let [first, rest @ ..] = bytes {
        if !is_blank(*first) {
            break;
        }
        bytes = rest;
    }
    while let [rest @ .., last] = bytes {
        if !is_blank(*last) {
            break;
        }
        bytes = rest;
    }
    bytes
}

/// Parse a file's head. `Ok(None)` when it is not a script; `Err(ENOEXEC)`
/// (a positive errno) when it starts with `#!` but the line is unusable.
pub fn parse(head: &[u8]) -> Result<Option<Interp<'_>>, u64> {
    let Some(body) = head.strip_prefix(b"#!") else {
        return Ok(None);
    };
    let window = &body[..body.len().min(MAX_LINE - 2)];
    let line = match window.iter().position(|&b| b == b'\n') {
        Some(end) => &window[..end],
        // No newline in the window: fine only if the file ends there.
        None if body.len() <= window.len() => window,
        None => return Err(ENOEXEC),
    };
    if line.contains(&0) {
        return Err(ENOEXEC);
    }
    let line = trim(line);
    let split = line.iter().position(|&b| is_blank(b)).unwrap_or(line.len());
    let (path, rest) = line.split_at(split);
    if path.is_empty() {
        return Err(ENOEXEC);
    }
    let path = core::str::from_utf8(path).map_err(|_| ENOEXEC)?;
    let rest = trim(rest);
    let arg = (!rest.is_empty()).then_some(rest);
    Ok(Some(Interp { path, arg }))
}

/// `bytes` with the NUL terminator argv entries carry.
fn c_arg(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len() + 1);
    out.extend_from_slice(bytes);
    out.push(0);
    out
}

/// Map special process paths to a real file (`/proc/self/exe` ->
/// `/system/bin/busybox`) and drop the leading `/` the ABI VFS lookups take
/// without.
fn exe_target(path: &str) -> &str {
    match path {
        "/proc/self/exe" => fhs::bin::BUSYBOX.trim_start_matches('/'),
        other => other.trim_start_matches('/'),
    }
}

/// Load one hop: refuse a `noexec` mount or a missing execute bit, then read
/// the file. Applet aliases and paths with no VFS node fall through to
/// [`load_executable`]. Root needs an `x` bit like everyone else, and a
/// directory is `EACCES`, as on Linux.
fn load_checked(target: &str) -> Result<Vec<u8>, u64> {
    if crate::fs::abi_mount_flags(target).noexec {
        return Err(fs_err(FsError::Access));
    }
    match crate::fs::abi_check(Id::current(), target, vfs::EXECUTE) {
        Ok(meta) if meta.kind != vfs::FileKind::File => return Err(fs_err(FsError::Access)),
        Ok(_) => {}
        // No node: `target` is a synthetic applet name (`sh`, `bin/ls`,
        // `rhai`), which `load_executable` maps to a build-placed 0755 file
        // (BusyBox or a program at the image root), or a missing file it
        // reports as `ENOENT`. Every name that is a node was checked above.
        Err(FsError::NotFound) => {}
        Err(error) => return Err(fs_err(error)),
    }
    match load_executable(target) {
        Ok(bytes) => Ok(bytes),
        Err(FsError::NotFound) => Err(err(ENOENT)),
        Err(error) => Err(fs_err(error)),
    }
}

/// Follow `path` through any `#!` lines to the ELF that will run, rewriting
/// argv at each hop. `name` is the program as the caller spelled it, which is
/// what the interpreter receives as the script argument (so `$0` reads
/// `./s.sh`, like Linux). Errors are `-errno`, ready to return.
pub fn resolve(mut path: String, mut name: Vec<u8>, mut argv: Vec<Vec<u8>>) -> Result<Image, u64> {
    for depth in 0..=MAX_DEPTH {
        let bytes = load_checked(exe_target(&path))?;
        let interp = match parse(&bytes) {
            Ok(Some(interp)) => interp,
            Ok(None) => return Ok(Image { elf: bytes, argv }),
            Err(code) => return Err(err(code)),
        };
        if depth == MAX_DEPTH {
            break;
        }
        let mut next = Vec::with_capacity(argv.len() + 2);
        next.push(c_arg(interp.path.as_bytes()));
        if let Some(arg) = interp.arg {
            next.push(c_arg(arg));
        }
        next.push(c_arg(&name));
        next.extend(argv.drain(..).skip(1));
        argv = next;
        name = interp.path.as_bytes().to_vec();
        path = resolve_at(AT_FDCWD, interp.path)?;
    }
    Err(err(ELOOP))
}
