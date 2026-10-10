//! A native program `execve`d from a shell reads the shell's stdin (issue
//! #315): in the desktop Terminal the keystrokes arrive on a pipe, not the
//! kernel key queue, so `read_char` must follow descriptor 0.

use super::*;
use crate::task::FdKind;

const SYS_CLOSE: u64 = 3;
const SYS_PIPE: u64 = 22;
const SYS_DUP2: u64 = 33;
const SYS_READ_CHAR: u64 = 2;

/// Bytes written to the shell's stdin pipe come back one at a time from the
/// native `read_char`; once every writer is gone it reports end of input
/// (a newline) instead of blocking forever, and a task whose fd 0 is still the
/// terminal is left to the key-queue path.
pub fn read_char_follows_redirected_stdin() -> Result<(), String> {
    fresh();
    let sh = shell()?;
    check!(
        native::read_redirected().is_none(),
        "a terminal stdin must use the key queue"
    );

    let mut fds = [0i32; 2];
    let ret = process::linux::dispatch_for_test(SYS_PIPE, fds.as_mut_ptr() as u64, 0, 0);
    check!(ret == 0, "pipe returned {ret:#x}");
    let (read_end, write_end) = (fds[0] as u64, fds[1] as u64);
    let ret = process::linux::dispatch_for_test(SYS_DUP2, read_end, 0, 0);
    check!(ret == 0, "dup2 returned {ret:#x}");
    let text = b"hi\n";
    let wrote =
        process::linux::dispatch_for_test(1, write_end, text.as_ptr() as u64, text.len() as u64);
    check!(
        wrote == text.len() as u64,
        "stdin pipe write returned {wrote}"
    );

    let child = native::spawn(fhs::bin::HELLO, &service_suite::minimal_elf(), &["hello"])
        .map_err(|e| format!("spawn errno {e}"))?;
    check!(
        task::harness::fd_kind_at(child, 0) == FdKind::Pipe,
        "the child did not inherit the redirected stdin"
    );
    task::harness::switch_current(child);
    for expected in *b"hi\n" {
        let got = process::dispatch_for_test(SYS_READ_CHAR, 0, 0, 0);
        check!(
            got == u64::from(expected),
            "read_char gave {got:#x}, wanted {expected:#x}"
        );
    }

    // Close every writer: the child's copy, then the shell's.
    process::linux::dispatch_for_test(SYS_CLOSE, write_end, 0, 0);
    task::harness::switch_current(sh);
    process::linux::dispatch_for_test(SYS_CLOSE, write_end, 0, 0);
    task::harness::switch_current(child);
    let eof = process::dispatch_for_test(SYS_READ_CHAR, 0, 0, 0);
    check!(
        eof == u64::from(b'\n') | 1 << 8,
        "end of input gave {eof:#x}, wanted a newline"
    );
    Ok(())
}

/// A `SOCK_SEQPACKET` stdin is treated as ended input and the queued message is
/// left intact: a one-byte stream read would truncate it and drop the rest.
pub fn read_char_leaves_seqpacket_messages_intact() -> Result<(), String> {
    fresh();
    shell()?;
    let mut sv = [0i32; 2];
    let ret = process::linux::dispatch_args_for_test(53, 1, 5, 0, sv.as_mut_ptr() as u64);
    check!(ret == 0, "socketpair(SEQPACKET) returned {ret:#x}");
    let (a, b) = (sv[0] as u64, sv[1] as u64);
    let ret = process::linux::dispatch_for_test(SYS_DUP2, a, 0, 0);
    check!(ret == 0, "dup2 returned {ret:#x}");
    let msg = b"whole message";
    let sent = process::linux::dispatch_for_test(1, b, msg.as_ptr() as u64, msg.len() as u64);
    check!(sent == msg.len() as u64, "seqpacket send returned {sent}");

    let got = process::dispatch_for_test(SYS_READ_CHAR, 0, 0, 0);
    check!(
        got == u64::from(b'\n') | 1 << 8,
        "seqpacket stdin gave {got:#x}, wanted the end-of-input newline"
    );
    let mut back = [0u8; 32];
    let n = process::linux::dispatch_for_test(0, 0, back.as_mut_ptr() as u64, back.len() as u64);
    check!(
        n == msg.len() as u64 && &back[..msg.len()] == msg,
        "the queued message was consumed or truncated: read {n}"
    );
    Ok(())
}
