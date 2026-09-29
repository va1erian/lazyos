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

    let child = native::spawn("HELLO.ELF", &service_suite::minimal_elf(), "")
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
        eof == u64::from(b'\n'),
        "end of input gave {eof:#x}, wanted a newline"
    );
    Ok(())
}
