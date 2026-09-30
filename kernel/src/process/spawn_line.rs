//! Parsing the command line of the `spawn` syscalls (6 and 10/`SPAWN`).
//!
//! The line is `"[linux:]PATH.ELF [args...]"`. Native programs (the default)
//! take their arguments through syscall 9; a `linux:` prefix selects the
//! Linux ABI personality, so `init` can supervise a static musl `std` program
//! (a desktop app) the same way it supervises a native service. The ELF header
//! alone cannot tell the two apart: both are static x86_64 executables linked
//! at the same base.
//!
//! A Linux program's arguments are split on whitespace, except that a token
//! written `"like this"` is one argument (no escapes, so it cannot contain a
//! `"`); `init` uses that to pass a file path with spaces as one `argv` item.

use alloc::string::String;
use alloc::vec::Vec;

/// The prefix that selects the Linux ABI personality.
pub const LINUX_PREFIX: &str = "linux:";

/// A parsed spawn command line.
#[derive(Debug, PartialEq, Eq)]
pub struct SpawnLine<'a> {
    /// Whether the program runs under the Linux ABI.
    pub linux: bool,
    /// The FAT file name.
    pub path: &'a str,
    /// The remainder of the line (may be empty).
    pub args: &'a str,
}

/// Parse `line`; `None` when it names no program.
pub fn parse(line: &str) -> Option<SpawnLine<'_>> {
    let line = line.trim();
    let (line, linux) = match line.strip_prefix(LINUX_PREFIX) {
        Some(rest) => (rest.trim_start(), true),
        None => (line, false),
    };
    if line.is_empty() {
        return None;
    }
    let (path, args) = match line.split_once(char::is_whitespace) {
        Some((path, args)) => (path, args.trim()),
        None => (line, ""),
    };
    Some(SpawnLine { linux, path, args })
}

/// Split `args` into `argv` items: whitespace-separated tokens, where a token
/// that starts with `"` runs to the next `"` (which must end the token).
/// `None` for an unterminated quote or a closing quote followed by more text,
/// so a malformed line is refused rather than guessed at.
pub fn argv(args: &str) -> Option<Vec<String>> {
    let mut items = Vec::new();
    let mut rest = args.trim_start();
    while !rest.is_empty() {
        let token = if let Some(quoted) = rest.strip_prefix('"') {
            let end = quoted.find('"')?;
            let (token, after) = (&quoted[..end], &quoted[end + 1..]);
            if !after.is_empty() && !after.starts_with(char::is_whitespace) {
                return None;
            }
            rest = after;
            token
        } else {
            let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
            let token = &rest[..end];
            rest = &rest[end..];
            token
        };
        items.push(String::from(token));
        rest = rest.trim_start();
    }
    Some(items)
}
