//! Parsing the command line of the `spawn` syscalls (6 and 10/`SPAWN`).
//!
//! The line is `"[linux:]PATH.ELF [args...]"`. Native programs (the default)
//! take their arguments through syscall 9; a `linux:` prefix selects the
//! Linux ABI personality, so `init` can supervise a static musl `std` program
//! (a desktop app) the same way it supervises a native service. The ELF header
//! alone cannot tell the two apart: both are static x86_64 executables linked
//! at the same base.

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
