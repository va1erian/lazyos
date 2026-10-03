//! `spawnv` (syscall 31) for a labelled spawn that hands the child its
//! standard streams: how an IDE that is itself a package runs the project it
//! edits under `dev:<system_name>` and still reads its output (issue #529,
//! `kernel/src/ipc/devspawn.rs`).
//!
//! Mirrors `user/src/sys/spawn.rs` (the request block of
//! `kernel/src/process/spawnv.rs`) for a static-musl program, which reaches the
//! native gate the same way. Only the one shape this crate needs is offered: a
//! Linux-ABI child, the caller's credential with a label string, and three
//! descriptors as its stdin, stdout and stderr.

use super::cred::Cred;
use core::arch::asm;

/// `spawnv(req)` — the argv-vector spawn.
pub const SYS_SPAWNV: u64 = 31;
/// The request's `personality` word: a Linux-ABI program.
const PERSONALITY_LINUX: u64 = 1;
/// Flag in the `personality` word: words 17-19 carry the child's streams.
const PERSONALITY_STDIO: u64 = 1 << 8;
/// The request's `cred` word: stamp the credential and the label string.
const CRED_AS_LABELLED: u64 = 2;
/// A stdio word that leaves the child's descriptor on the terminal.
const STDIO_TERMINAL: u64 = u64::MAX;

/// Start the static Linux program at `path` as a child of this task, stamped
/// with `cred` (uid, gid, session and capabilities; its `label_id` is
/// ignored) and the label `label`. `stdio[i]` is this task's descriptor that
/// becomes the child's descriptor `i` (`None`: the terminal). Returns the
/// child's pid, or the negative errno (`-EACCES` when the kernel refuses the
/// label, `-EBADF` for a closed descriptor).
pub fn spawn_labelled(
    path: &str,
    argv: &[&str],
    envp: &[&str],
    cred: Cred,
    label: &str,
    stdio: [Option<i32>; 3],
) -> Result<u64, i64> {
    let argv_block = block(argv);
    let envp_block = block(envp);
    let words = cred.to_words();
    let stream = |fd: Option<i32>| fd.map_or(STDIO_TERMINAL, |fd| fd as u64);
    let request: [u64; 20] = [
        path.as_ptr() as u64,
        path.len() as u64,
        argv_block.as_ptr() as u64,
        argv_block.len() as u64,
        argv.len() as u64,
        envp_block.as_ptr() as u64,
        envp_block.len() as u64,
        envp.len() as u64,
        PERSONALITY_LINUX | PERSONALITY_STDIO,
        CRED_AS_LABELLED,
        words[0],
        words[1],
        words[2],
        words[3],
        words[4],
        label.as_ptr() as u64,
        label.len() as u64,
        stream(stdio[0]),
        stream(stdio[1]),
        stream(stdio[2]),
    ];
    let code: u64;
    // SAFETY: `int 0x80` with syscall 31 and the native convention (number in
    // rax, the request pointer in rdi, result in rax; rcx/r11 clobbered). The
    // request and every buffer it points at live on this frame until the call
    // returns, and the kernel validates each pointer against this task.
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

/// Each string followed by a NUL.
fn block(items: &[&str]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(items.iter().map(|item| item.len() + 1).sum());
    for item in items {
        bytes.extend_from_slice(item.as_bytes());
        bytes.push(0);
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_block_is_each_string_and_a_nul() {
        assert_eq!(block(&["a b", "", "c"]), b"a b\0\0c\0");
        assert!(block(&[]).is_empty());
    }
}
