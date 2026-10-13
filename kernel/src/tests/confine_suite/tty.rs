//! A confined task reads end of file from a terminal (`sys_read`): the
//! keystrokes of the shell its launcher was started from are not the
//! sandbox's, whichever descriptor they arrive on.

use super::*;
use crate::task::consoletty;

const SYS_READ: u64 = 0;
const SYS_FCNTL: u64 = 72;
const F_SETFL: u64 = 4;
const O_NONBLOCK: u64 = 0o4000;
const EAGAIN: i64 = -11;

/// Type `text` and a newline at the console, as the keyboard path would
/// have the discipline see it, after discarding anything already there.
fn type_at_console(text: &str) {
    let (_, _fed) = consoletty::with_console(|ldisc| {
        let mut scratch = [0u8; 256];
        while ldisc.readable() {
            if ldisc.read(&mut scratch) == 0 {
                break;
            }
        }
        let mut echo = Vec::new();
        for byte in text.bytes().chain(core::iter::once(b'\n')) {
            let _ = ldisc.input(byte, &mut echo);
        }
    });
}

fn console_readable() -> bool {
    consoletty::with_console(|ldisc| ldisc.readable()).0
}

fn read(fd: i64, buf: &mut [u8]) -> i64 {
    process::linux::dispatch_for_test(
        SYS_READ,
        fd as u64,
        buf.as_mut_ptr() as u64,
        buf.len() as u64,
    ) as i64
}

/// Terminal and pty reads: a task the label confines gets `0` (and leaves the
/// input where it was), and the same task unlabelled, or a process label that
/// holds no marker at all, is judged as it should be.
pub fn terminal_reads_are_eof() -> Result<(), String> {
    with_world(|| {
        as_plain_user();
        let console = openat("/dev/console", O_RDWR);
        let master = openat("/dev/ptmx", O_RDWR);
        check!(console >= 3 && master >= 3, "open: {console}, {master}");
        // Standard streams that are terminals too (the launcher's).
        let streams: Vec<i64> = (0..3)
            .filter(|fd| task::fd_kind(*fd as usize) == task::FdKind::Terminal)
            .collect();
        let nonblock =
            process::linux::dispatch_for_test(SYS_FCNTL, master as u64, F_SETFL, O_NONBLOCK) as i64;
        check!(nonblock == 0, "O_NONBLOCK on the pty: {nonblock}");

        // The control: an unconfined task reads what was typed, and an idle
        // pty tells it to try again.
        type_at_console("hi");
        let mut buf = [0u8; 16];
        let mut idle = [0u8; 16];
        check!(
            read(master, &mut idle) == EAGAIN,
            "an unconfined idle pty read did not report EAGAIN"
        );

        // Confined by the marker: end of file from every terminal descriptor,
        // and the typed line is still there.
        let label = confine_as_user(RULES, &[])?;
        check!(confine::slot_confined(task::KERNEL_TASK), "not confined");
        for fd in [console, master].into_iter().chain(streams.iter().copied()) {
            let got = read(fd, &mut buf);
            check!(got == 0, "a confined read of terminal {fd} gave {got}");
        }
        check!(
            console_readable(),
            "a confined read consumed the launcher's keystrokes"
        );

        // A process label (`app:<id>/<name>`) is confined without a marker.
        let child = labels::intern(&format!("{LABEL}/page")).map_err(|e| format!("{e:?}"))?;
        check!(child != label, "setup: the child is the app");
        credentials::set(
            task::KERNEL_TASK,
            Cred::new(USER_CRED.0, USER_CRED.1, 0, child, 0),
        );
        check!(
            confine::slot_confined(task::KERNEL_TASK),
            "child not confined"
        );
        let got = read(console, &mut buf);
        check!(got == 0, "a process label read the console: {got}");
        let got = read(master, &mut idle);
        check!(got == 0, "a process label read the pty: {got}");

        // Unlabelled again: the same descriptors deliver, so the zeroes
        // above were the label's.
        as_plain_user();
        let got = read(console, &mut buf);
        check!(
            got == 3 && &buf[..3] == b"hi\n",
            "an unconfined console read gave {got}: {:?}",
            &buf[..4]
        );
        check!(
            read(master, &mut idle) == EAGAIN,
            "an unconfined idle pty read changed"
        );
        close(console);
        close(master);
        Ok(())
    })
}
