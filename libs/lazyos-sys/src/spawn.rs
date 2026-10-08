//! The argv-vector spawn (`spawnv`, syscall 31, fs F3): the request block of
//! `kernel/src/process/spawnv.rs`.

use alloc::vec::Vec;

use crate::cred::Cred;
use crate::{nr, value};

/// Which ABI a [`spawnv`] child runs under.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Personality {
    /// A native LazyOS program: it reads its arguments with syscall 9.
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
    /// (`CAP_SETUID`, never wider than the caller): the login path.
    As(Cred),
    /// Stamped with the credential and the label string (`app:<name>`,
    /// `system:<name>` or `dev:<name>`, interned by the kernel; the
    /// credential's `label_id` is ignored). Only an unlabelled `CAP_SETUID`
    /// holder (or one already in that label) may; the exception is a
    /// labelled IDE spawning into an approved `dev:` label
    /// (`kernel/src/ipc/devspawn.rs`).
    AsLabelled(Cred, &'a str),
}

/// A child's standard streams: `stdio[i]` is this task's descriptor that
/// becomes the child's descriptor `i`; `None` leaves it on the terminal.
pub type Stdio = [Option<i32>; 3];

/// Longest path the kernel accepts (`-ENAMETOOLONG` past it).
pub const SPAWN_PATH_MAX: usize = 255;
/// Largest `argv` or `envp` block: the strings plus one NUL each (`-E2BIG`).
pub const SPAWN_BLOCK_MAX: usize = 4096;
/// Most `argv` or `envp` strings.
pub const SPAWN_COUNT_MAX: usize = 64;

/// The `personality` word of a Linux-ABI child.
const PERSONALITY_LINUX: u64 = 1;
/// Flag in the `personality` word: words 17-19 carry the child's streams.
const PERSONALITY_STDIO: u64 = 1 << 8;
/// A stdio word that leaves the child's descriptor on the terminal.
const STDIO_TERMINAL: u64 = u64::MAX;
/// Words of a request with streams; one without stops at 17.
pub const REQUEST_WORDS: usize = 20;

/// Each string followed by a NUL: the kernel's `argv`/`envp` block.
pub fn block(items: &[&str]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(items.iter().map(|item| item.len() + 1).sum());
    for item in items {
        bytes.extend_from_slice(item.as_bytes());
        bytes.push(0);
    }
    bytes
}

/// The request block for `path` with the blocks of `argv` (`argv_count`
/// strings) and `envp`. Every pointer in it borrows from the arguments, so
/// it is only meaningful while they live.
pub fn request(
    path: &str,
    (argv, argv_count): (&[u8], usize),
    (envp, envp_count): (&[u8], usize),
    personality: Personality,
    cred: SpawnCred<'_>,
    stdio: Option<Stdio>,
) -> [u64; REQUEST_WORDS] {
    let (mode, words, label) = match cred {
        SpawnCred::Inherit => (0, [0; 5], ""),
        SpawnCred::As(cred) => (1, cred.to_words(), ""),
        SpawnCred::AsLabelled(cred, label) => (2, cred.to_words(), label),
    };
    let mut kind = match personality {
        Personality::Native => 0,
        Personality::Linux => PERSONALITY_LINUX,
    };
    let stream = |fd: Option<i32>| fd.map_or(STDIO_TERMINAL, |fd| fd as u64);
    let streams = match stdio {
        Some(stdio) => {
            kind |= PERSONALITY_STDIO;
            stdio.map(stream)
        }
        None => [0; 3],
    };
    [
        path.as_ptr() as u64,
        path.len() as u64,
        argv.as_ptr() as u64,
        argv.len() as u64,
        argv_count as u64,
        envp.as_ptr() as u64,
        envp.len() as u64,
        envp_count as u64,
        kind,
        mode,
        words[0],
        words[1],
        words[2],
        words[3],
        words[4],
        label.as_ptr() as u64,
        label.len() as u64,
        streams[0],
        streams[1],
        streams[2],
    ]
}

/// Start the program at `path` as a child of the calling task, with `argv`
/// (`argv[0]` included, conventionally the program name) and `envp`
/// (`KEY=VALUE` strings). Nothing is split: a path or an argument may contain
/// spaces. `stdio` hands the child descriptors (see [`Stdio`]). Returns the
/// child's pid, or the negative errno (`-EACCES` for a refused label,
/// `-EBADF` for a closed descriptor).
pub fn spawnv_stdio(
    path: &str,
    argv: &[&str],
    envp: &[&str],
    personality: Personality,
    cred: SpawnCred<'_>,
    stdio: Option<Stdio>,
) -> Result<u64, i64> {
    let argv_block = block(argv);
    let envp_block = block(envp);
    let request = request(
        path,
        (&argv_block, argv.len()),
        (&envp_block, envp.len()),
        personality,
        cred,
        stdio,
    );
    // SAFETY: the kernel reads the request and the path, blocks and label it
    // points at; all of them live on this frame until the call returns.
    value(unsafe { crate::raw::syscall1(nr::SPAWNV, request.as_ptr() as u64) })
}

/// [`spawnv_stdio`] with the child's streams left on the terminal.
pub fn spawnv(
    path: &str,
    argv: &[&str],
    envp: &[&str],
    personality: Personality,
    cred: SpawnCred<'_>,
) -> Result<u64, i64> {
    spawnv_stdio(path, argv, envp, personality, cred, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_block_is_each_string_and_a_nul() {
        assert_eq!(block(&["a b", "", "c"]), b"a b\0\0c\0");
        assert!(block(&[]).is_empty());
    }

    #[test]
    fn an_inheriting_native_request_has_no_streams() {
        let argv = block(&["/system/bin/x", "-v"]);
        let words = request(
            "/system/bin/x",
            (&argv, 2),
            (&[], 0),
            Personality::Native,
            SpawnCred::Inherit,
            None,
        );
        assert_eq!(words[1], 13);
        assert_eq!((words[3], words[4]), (argv.len() as u64, 2));
        assert_eq!((words[6], words[7]), (0, 0));
        assert_eq!((words[8], words[9]), (0, 0));
        assert_eq!(&words[10..15], &[0; 5]);
        assert_eq!(words[16], 0);
    }

    #[test]
    fn a_labelled_linux_request_carries_cred_label_and_streams() {
        let cred = Cred::new(1000, 100, 0, 9, 3);
        let words = request(
            "p",
            (&[], 0),
            (&[], 0),
            Personality::Linux,
            SpawnCred::AsLabelled(cred, "dev:demo"),
            Some([Some(4), None, Some(2)]),
        );
        assert_eq!(words[8], PERSONALITY_LINUX | PERSONALITY_STDIO);
        assert_eq!(words[9], 2);
        assert_eq!(&words[10..15], &cred.to_words());
        assert_eq!(words[16], 8);
        assert_eq!(&words[17..], &[4, STDIO_TERMINAL, 2]);
    }

    #[test]
    fn a_set_credential_is_mode_one_without_a_label() {
        let cred = Cred::new(5, 5, 0, 0, 1);
        let words = request(
            "p",
            (&[], 0),
            (&[], 0),
            Personality::Native,
            SpawnCred::As(cred),
            None,
        );
        assert_eq!((words[9], words[16]), (1, 0));
        assert_eq!(&words[10..15], &cred.to_words());
    }
}
