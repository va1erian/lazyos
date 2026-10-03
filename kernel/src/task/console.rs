//! The kernel console: output, keyboard input and focus.

use super::*;

/// Append output to the current process's terminal, dropping ANSI escape
/// sequences (our window renderer has no terminal emulation yet). Forked
/// children write to their root ancestor's window.
pub fn write_output(bytes: &[u8]) {
    let mut tasks = TASKS.lock();
    let root = root_index(&tasks);
    if let Some(task) = tasks[root].as_mut() {
        strip_ansi(bytes, &mut task.output);
    }
    drop(tasks);
    NEEDS_REDRAW.store(true, Ordering::Relaxed);
}

/// Walk the parent chain to the process leader (the task with no parent).
pub(super) fn root_index(tasks: &[Option<Task>; MAX_TASKS]) -> usize {
    root_of(tasks, current())
}

/// The window owner of `slot` (see [`root_index`]).
pub(super) fn root_of(tasks: &[Option<Task>; MAX_TASKS], slot: usize) -> usize {
    let mut index = slot;
    while let Some(task) = tasks[index].as_ref() {
        // A thread's terminal is its process's: follow the thread group to
        // its leader, then the parents.
        match (task.parent, task.linux.tgid) {
            (0, 0) => break,
            (0, leader) if leader != index => index = leader,
            (0, _) => break,
            (parent, _) => index = parent,
        }
    }
    index
}

/// Copy `bytes` into `out`, applying just enough terminal control for an
/// interactive shell: `\r`, backspace, erase-to-end-of-line, cursor-left, and
/// dropping other CSI sequences.
pub(super) fn strip_ansi(bytes: &[u8], out: &mut Vec<u8>) {
    let mut i = 0;
    while i < bytes.len() {
        let byte = bytes[i];
        if byte == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b'[' {
            i += 2;
            let start = i;
            while i < bytes.len() && !(0x40..=0x7e).contains(&bytes[i]) {
                i += 1;
            }
            if i < bytes.len() {
                let count = parse_count(&bytes[start..i]);
                match bytes[i] {
                    b'K' => truncate_line(out),
                    b'D' => {
                        for _ in 0..count {
                            out.pop();
                        }
                    }
                    b'J' if count == 2 => out.clear(),
                    _ => {}
                }
                i += 1; // final byte
            }
            continue;
        }
        match byte {
            b'\r' => truncate_line(out),
            0x08 => {
                out.pop();
            }
            _ => out.push(byte),
        }
        i += 1;
    }
}

/// Parse a CSI parameter (defaults to 1 when empty).
pub(super) fn parse_count(digits: &[u8]) -> usize {
    let mut value = 0usize;
    let mut any = false;
    for &d in digits {
        if d.is_ascii_digit() {
            value = value * 10 + (d - b'0') as usize;
            any = true;
        }
    }
    if any {
        value
    } else {
        1
    }
}

/// Discard the current (last) line's contents.
pub(super) fn truncate_line(out: &mut Vec<u8>) {
    match out.iter().rposition(|&c| c == b'\n') {
        Some(pos) => out.truncate(pos + 1),
        None => out.clear(),
    }
}

/// Pop a key for the current process (its root ancestor's queue).
pub fn take_key() -> Option<Key> {
    let mut tasks = TASKS.lock();
    let root = root_index(&tasks);
    tasks[root].as_mut().and_then(|task| task.input.pop_front())
}

/// Route a decoded key: Tab cycles focus, others go to the focused task.
pub fn on_key(key: Key) {
    if key == Key::Tab {
        cycle_focus();
        return;
    }
    // Emulate the terminal line discipline's INTR character. There is no tty
    // layer to signal the foreground group, so Ctrl-C (ETX) is intercepted here
    // and becomes SIGINT for the focused task's process group. This is what
    // lets BusyBox `sh` interrupt a running child.
    // With `ISIG` off (a raw-mode program) it is an ordinary byte.
    if key == Key::Char('\u{3}') {
        if let Some(target) = consoletty::console_interrupt_target(FOCUS.load(Ordering::Relaxed)) {
            crate::tty::signal_console(target, signal::SIGINT);
            return;
        }
    }
    let focus = FOCUS.load(Ordering::Relaxed);
    {
        let mut tasks = TASKS.lock();
        if let Some(task) = tasks[focus].as_mut() {
            task.input.push_back(key);
        }
    }
    // Wake blocked readers after releasing the task table: wait queues take the
    // task table inside notify, so the lock order is always queue -> task.
    // Readers that got no key just park again (spurious wakeup).
    INPUT_GEN.fetch_add(1, Ordering::AcqRel);
    wait::TERMINAL.notify_all();
    notify_poll();
}

/// Inject bytes into the current process's input queue (e.g. a terminal reply).
pub fn inject_input(bytes: &[u8]) {
    {
        let mut tasks = TASKS.lock();
        let root = root_index(&tasks);
        if let Some(task) = tasks[root].as_mut() {
            for &byte in bytes {
                task.input.push_back(Key::Char(byte as char));
            }
        }
    }
    INPUT_GEN.fetch_add(1, Ordering::AcqRel);
    wait::TERMINAL.notify_all();
    notify_poll();
}

/// Whether the current process has pending terminal input.
pub fn input_available() -> bool {
    let tasks = TASKS.lock();
    let root = root_index(&tasks);
    tasks[root]
        .as_ref()
        .map(|task| !task.input.is_empty())
        .unwrap_or(false)
}

/// Freshness counter for terminal input (see [`Fd::poll_gen`]).
pub fn input_gen() -> u64 {
    INPUT_GEN.load(Ordering::Acquire)
}

pub(super) fn cycle_focus() {
    let tasks = TASKS.lock();
    let start = FOCUS.load(Ordering::Relaxed);
    for step in 1..=MAX_TASKS {
        let candidate = (start + step) % MAX_TASKS;
        if candidate == KERNEL_TASK {
            continue;
        }
        if let Some(task) = tasks[candidate].as_ref() {
            if task.state != TaskState::Done {
                FOCUS.store(candidate, Ordering::Relaxed);
                NEEDS_REDRAW.store(true, Ordering::Relaxed);
                return;
            }
        }
    }
}

/// The focused task index.
pub fn focus() -> usize {
    FOCUS.load(Ordering::Relaxed)
}
