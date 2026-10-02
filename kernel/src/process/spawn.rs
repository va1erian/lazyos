//! Native `spawn`: name interning and the command-line spawn gate.

use super::creds::*;
use super::*;

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
    // The path is caller-supplied, so distinct spellings are not bounded by
    // the manifest: `./A.ELF` and `A.ELF` name one file but intern twice, and
    // a caller can invent spellings that fail to resolve. (The root volume is
    // ext2, which is case-sensitive, so `a.elf` is a different, missing file.)
    // Past the cap every new spelling shares one generic name instead of
    // leaking another string.
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
/// the file name (a path on the root volume), the remainder is split into the
/// `argv` block syscall 9 returns. Returns the new
/// task's pid (its slot), or `u64::MAX` when the file is missing, the ELF is
/// invalid, or no slot/frame is free.
pub(super) fn sys_spawn(cmdline_ptr: u64) -> u64 {
    let code = spawn_program(cmdline_ptr, None, false);
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
/// the gate approved. `assign_label` is the labelled-spawn flavour: the
/// request's `label_id` is given to the child instead of having to match the
/// inherited one. Negative return values are errno codes; a positive
/// value is the new child's pid.
pub(super) fn spawn_program(cmdline_ptr: u64, cred: Option<Cred>, assign_label: bool) -> i64 {
    let Ok(line) = user_cstr(cmdline_ptr) else {
        return -EFAULT;
    };
    let Some(spawn_line::SpawnLine { linux, path, args }) = spawn_line::parse(&line) else {
        return -EINVAL;
    };
    if fs::mount_flags(path).noexec {
        return -EACCES;
    }
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
        // argv[0] is the program name; the rest are the split args (a
        // double-quoted token is one item, see `spawn_line::argv`).
        let Some(items) = spawn_line::argv(args) else {
            return -EINVAL;
        };
        let argv: Vec<&str> = core::iter::once(path)
            .chain(items.iter().map(String::as_str))
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
        let label = if assign_label {
            credentials::LabelStamp::Assign
        } else {
            credentials::LabelStamp::Keep {
                current: credentials::of(slot).label_id,
            }
        };
        let _ = credentials::transition_with(task::current(), slot, cred, label);
    }
    // syscall 9 hands a native child its `argv` block (`argstore`).
    super::argstore::set_legacy(slot, path, args.as_bytes());
    slot as i64
}
