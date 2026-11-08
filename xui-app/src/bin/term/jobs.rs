//! Whether a program other than the shell is running in the Terminal, so
//! closing the window can ask first. Two signs: the pty's foreground process
//! group (`TIOCGPGRP` on the master) is not the shell's, which is what a shell
//! with job control does for a command; or the shell has a live child task
//! (the kernel's task table), which is what BusyBox's ash shows when it runs
//! a command in its own group (it has no job control on this tty).

use std::os::unix::io::RawFd;

/// What the shell is doing, from the kernel's task table.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ShellState {
    /// A live task has the shell as its parent.
    pub children: bool,
    /// The shell is parked waiting for input (the terminal or a poll set), as
    /// it is at its prompt (or looping on the tty, which shows as running);
    /// parked on a timer or a child's exit means it is doing something.
    pub waiting_for_input: bool,
    /// The row's state and wait labels, for the evidence line.
    pub detail: (&'static str, &'static str),
}

/// Whether a program other than the shell is running: `foreground` is what the
/// master reported (`None` when the question failed), `shell` the shell's own
/// group (it leads its session, so its pid), `state` what the task table says.
pub fn busy(foreground: Option<i32>, shell: i32, state: Option<ShellState>) -> bool {
    let other_group = matches!(foreground, Some(group) if group > 0 && group != shell);
    other_group || state.is_some_and(|s| s.children || !s.waiting_for_input)
}

/// The shell's state from a snapshot of the task table, if it is there.
pub fn shell_state(shell: u64) -> Option<ShellState> {
    use xui_app::sysinfo::{TaskState, WaitKind};
    let snapshot = xui_app::sysinfo::snapshot().ok()?;
    let row = snapshot.live_tasks().find(|t| t.pid == shell)?;
    let children = snapshot.live_tasks().any(|t| t.ppid == shell);
    // ash at its prompt reads the tty in a loop the kernel shows as running (or
    // parked on the terminal or a poll set); a command it runs itself (`sleep`)
    // parks it on a timer, and one it waits for on a child's exit.
    let waiting_for_input = row.state != TaskState::Blocked
        || matches!(row.wait, WaitKind::None | WaitKind::Terminal | WaitKind::Poll);
    Some(ShellState { children, waiting_for_input, detail: (row.state.label(), row.wait.label()) })
}

/// The master's foreground process group, if the kernel will say.
pub fn foreground_group(master: RawFd) -> Option<i32> {
    let mut group: libc::pid_t = 0;
    // SAFETY: `TIOCGPGRP` writes one `pid_t` through a valid pointer.
    let rc = unsafe { libc::ioctl(master, libc::TIOCGPGRP, &mut group as *mut libc::pid_t) };
    (rc == 0).then_some(group)
}

#[cfg(test)]
mod tests {
    use super::*;

    const IDLE: ShellState = ShellState { children: false, waiting_for_input: true, detail: ("blocked", "tty") };

    #[test]
    fn the_shell_at_its_prompt_is_not_busy() {
        assert!(!busy(Some(100), 100, Some(IDLE)));
    }

    #[test]
    fn another_group_in_front_is_busy() {
        assert!(busy(Some(250), 100, Some(IDLE)));
        assert!(busy(Some(1), 100, None));
    }

    #[test]
    fn a_child_of_the_shell_is_a_running_program_even_in_its_own_group() {
        let child = ShellState { children: true, waiting_for_input: false, detail: ("", "") };
        assert!(busy(Some(100), 100, Some(child)), "ash runs commands in the shell's group");
        assert!(busy(None, 100, Some(child)));
    }

    #[test]
    fn a_shell_busy_itself_is_running_a_command_in_process() {
        // BusyBox runs `sleep` and other applets inside the shell.
        let sleeping = ShellState { children: false, waiting_for_input: false, detail: ("", "") };
        assert!(busy(Some(100), 100, Some(sleeping)));
    }

    #[test]
    fn an_unanswered_question_does_not_block_closing() {
        assert!(!busy(None, 100, None));
        assert!(!busy(Some(0), 100, None));
        assert!(!busy(Some(-1), 100, None));
        assert!(!busy(None, 100, Some(IDLE)));
    }

    #[test]
    fn a_bad_descriptor_reports_nothing() {
        assert_eq!(foreground_group(-1), None);
    }
}
