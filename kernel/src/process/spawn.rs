//! Native `spawn`: service argument strings, name interning and the spawn gate.

use super::creds::*;
use super::*;

/// Service argument strings, keyed by task slot (issue #93).
///
/// Native programs receive no `argv`/`argc` stack, so `spawn` stores the
/// manifest argument string here and syscall 9 (or `sys::service_args`) copies
/// it out. The entry is overwritten on the slot's next state-changing spawn and
/// only read by that slot, so a re-used slot cannot observe stale arguments of
/// a *different* program (a plain kernel `spawn` clears the slot).
pub(super) static SERVICE_ARGS: Mutex<[Option<Vec<u8>>; task::MAX_TASKS]> =
    Mutex::new([const { None }; task::MAX_TASKS]);

/// Distinct spellings [`intern_service_name`] will leak before it falls back to
/// [`OVERFLOW_NAME`]; a real manifest has a few dozen services.
pub(super) const MAX_INTERNED_NAMES: usize = 64;
/// The task name given to programs whose spelling arrives after the intern
/// table is full.
pub(super) const OVERFLOW_NAME: &str = "service";

/// Intern a userspace-provided service name into a `&'static str` for
/// [`task::spawn_child`].
///
/// `Task::name` is `&'static str`, but the name comes from the supervisor's
/// manifest at runtime. Leaking each *distinct* name once (bounded by the
/// manifest, not by restart count) is the smallest way to satisfy that type
/// without adding an allocation policy to the task table.
pub(super) fn intern_service_name(name: &str) -> &'static str {
    static NAMES: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());
    let mut names = NAMES.lock();
    if let Some(known) = names.iter().find(|known| **known == name) {
        return known;
    }
    // The path is caller-supplied and the file system is case-insensitive
    // (`A.ELF`, `a.elf`, `./A.ELF` name one file), so distinct spellings are
    // not bounded by the manifest. Past the cap every new spelling shares one
    // generic name instead of leaking another string.
    if names.len() >= MAX_INTERNED_NAMES {
        return OVERFLOW_NAME;
    }
    let leaked: &'static str = Box::leak(String::from(name).into_boxed_str());
    names.push(leaked);
    leaked
}

/// syscall 6: start `"PATH.ELF [args...]"` as a child of the calling task.
///
/// The command line is NUL-terminated. The first whitespace-separated token is
/// the FAT file name, the remainder is stored for syscall 9. Returns the new
/// task's pid (its slot), or `u64::MAX` when the file is missing, the ELF is
/// invalid, or no slot/frame is free.
pub(super) fn sys_spawn(cmdline_ptr: u64) -> u64 {
    let code = spawn_program(cmdline_ptr, None);
    if code < 0 {
        u64::MAX
    } else {
        code as u64
    }
}

/// The shared body of syscalls 6 and 10 (`spawn` and the credentialed spawn).
///
/// `cred` is `Some` only on the credential-gate path, where the caller has
/// already validated the request with [`credentials::check`]. The child is
/// created holding a copy of the *caller's* credentials (`task::spawn_child`
/// stamps them), never the root default: a task can only start children that
/// are no more privileged than itself. On the credential-gate path the
/// requested credential is then stamped while interrupts are off in the
/// `int 0x80` gate, so the child never runs with any identity but the one
/// the gate approved. Negative return values are errno codes; a positive
/// value is the new child's pid.
pub(super) fn spawn_program(cmdline_ptr: u64, cred: Option<Cred>) -> i64 {
    let Ok(line) = user_cstr(cmdline_ptr) else {
        return -EFAULT;
    };
    let Some(spawn_line::SpawnLine { linux, path, args }) = spawn_line::parse(&line) else {
        return -EINVAL;
    };
    // A Linux program may be a BusyBox applet alias (`sh`, `/bin/ls`), which the
    // Linux loader resolves to the `BUSYBOX` file; a native program is always a
    // real FAT entry.
    let elf = if linux {
        linux::load_executable(path).or_else(|| fs::read(path))
    } else {
        fs::read(path)
    };
    let Some(elf) = elf else {
        return -ENOENT;
    };
    let name = intern_service_name(path);
    let started = if linux {
        // argv[0] is the program name; the rest are the whitespace-split args.
        let argv: Vec<&str> = core::iter::once(path)
            .chain(args.split_whitespace())
            .collect();
        task::spawn_linux_child(name, &elf, &argv)
    } else {
        task::spawn_child(name, &elf)
    };
    let slot = match started {
        Ok(slot) => slot,
        Err(_) => return -ENOMEM,
    };
    if let Some(cred) = cred {
        // `check` ran before the spawn, so this cannot fail; if it ever did,
        // the child would keep the identity it inherited from the caller (no
        // more privileged than the caller) and the gate would still audit the
        // refusal, which is the loudest signal available here.
        let _ = credentials::transition(task::current(), slot, cred);
    }
    SERVICE_ARGS.lock()[slot] = Some(args.as_bytes().to_vec());
    slot as i64
}
