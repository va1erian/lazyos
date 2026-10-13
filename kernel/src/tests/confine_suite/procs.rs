//! Sockets, process creation, signals and the calls a sandbox must never
//! gain by being implemented.

use super::*;
use crate::task::signal::{self, SigInfo, SignalError};
use crate::task::TaskState;

const SYS_SOCKET: u64 = 41;
const SYS_CONNECT: u64 = 42;
const SYS_BIND: u64 = 49;
const SYS_SOCKETPAIR: u64 = 53;
const SYS_CLONE: u64 = 56;
const SYS_FORK: u64 = 57;
const SYS_VFORK: u64 = 58;
const SYS_SPAWNV: u64 = 31;

const AF_UNIX: u64 = 1;
const AF_INET: u64 = 2;
const SOCK_STREAM: u64 = 1;
const SOCK_DGRAM: u64 = 2;
const CLONE_VM: u64 = 0x100;
const SIGCHLD: u64 = 17;

/// `struct sockaddr_un` for `name` (a path, or an abstract name whose first
/// byte is NUL) and the length to pass.
fn sockaddr_un(name: &[u8]) -> (Vec<u8>, u64) {
    let mut addr = Vec::new();
    addr.extend_from_slice(&(AF_UNIX as u16).to_le_bytes());
    addr.extend_from_slice(name);
    if name.first() != Some(&0) {
        addr.push(0);
    }
    let len = addr.len() as u64;
    (addr, len)
}

fn socket(domain: u64, kind: u64) -> i64 {
    process::linux::dispatch_for_test(SYS_SOCKET, domain, kind, 0) as i64
}

fn bind(fd: i64, name: &[u8]) -> i64 {
    let (addr, len) = sockaddr_un(name);
    process::linux::dispatch_for_test(SYS_BIND, fd as u64, addr.as_ptr() as u64, len) as i64
}

fn connect(fd: i64, name: &[u8]) -> i64 {
    let (addr, len) = sockaddr_un(name);
    process::linux::dispatch_for_test(SYS_CONNECT, fd as u64, addr.as_ptr() as u64, len) as i64
}

/// `AF_INET` and named `AF_UNIX` sockets follow the label's rules; an
/// anonymous pair needs none.
pub fn sockets() -> Result<(), String> {
    with_world(|| {
        crate::ipc::unix::clear_for_test();
        let label = confine_as_user(RULES, &[])?;

        for kind in [SOCK_STREAM, SOCK_DGRAM] {
            let got = socket(AF_INET, kind);
            check!(
                got == EPERM,
                "socket(AF_INET, {kind}) without the network rule gave {got}"
            );
        }

        // What the sandbox needs: an inherited, anonymous pair.
        let mut sv = [0i32; 2];
        let got = process::linux::dispatch_args_for_test(
            SYS_SOCKETPAIR,
            AF_UNIX,
            SOCK_STREAM,
            0,
            sv.as_mut_ptr() as u64,
        ) as i64;
        check!(got == 0, "socketpair gave {got}");
        close(i64::from(sv[0]));
        close(i64::from(sv[1]));

        // Named sockets are paths: a write grant is needed, and a name
        // outside the grants (another app's, the system's) is refused both
        // to bind and to connect, so nothing is reachable by name.
        let own = format!("{PROFILE_DIR}/ipc.sock");
        let fd = socket(AF_UNIX, SOCK_STREAM);
        check!(fd >= 0, "socket(AF_UNIX) gave {fd}");
        check!(
            bind(fd, own.as_bytes()) == 0,
            "bind inside the profile directory"
        );
        close(fd);
        for name in [
            "/tmp/.X11-unix/X0",
            "/home/user/.ssh/agent.sock",
            "/run/dbus/system_bus_socket",
            "relative.sock",
        ] {
            let fd = socket(AF_UNIX, SOCK_STREAM);
            check!(bind(fd, name.as_bytes()) == EPERM, "bind {name}");
            check!(connect(fd, name.as_bytes()) == EPERM, "connect {name}");
            close(fd);
        }
        let fd = socket(AF_UNIX, SOCK_STREAM);
        check!(
            bind(fd, b"\0abstract-name") == EPERM,
            "bind of an abstract name"
        );
        check!(
            connect(fd, b"\0abstract-name") == EPERM,
            "connect to an abstract name"
        );
        close(fd);

        // The plain user may use all of them.
        as_plain_user();
        let fd = socket(AF_UNIX, SOCK_STREAM);
        check!(bind(fd, b"/tmp/free.sock") == 0, "the plain user's bind");
        close(fd);
        let fd = socket(AF_UNIX, SOCK_STREAM);
        check!(bind(fd, b"\0free") == 0, "the plain user's abstract bind");
        close(fd);

        // `network = ["outbound"]` is the socket interface's rule.
        let network = messenger_generated::os_lazy_net_socket_v1::INTERFACE_ID;
        let mut ids =
            filepolicy::compile(RULES).map_err(|(i, e)| format!("{i}: {}", e.message()))?;
        ids.push(network);
        load_ids(label, &ids)?;
        credentials::set(
            task::KERNEL_TASK,
            Cred::new(USER_CRED.0, USER_CRED.1, 0, label, 0),
        );
        for kind in [SOCK_STREAM, SOCK_DGRAM] {
            let got = socket(AF_INET, kind);
            check!(
                got >= 0,
                "socket(AF_INET, {kind}) with the network rule gave {got}"
            );
            close(got);
        }
        // The rule grants sockets, not files: the same label is still held.
        check!(
            !opens("/home/user/.ssh/id_rsa", O_RDONLY),
            "the network rule opened a file"
        );
        crate::ipc::unix::clear_for_test();
        Ok(())
    })
}

const PROFILE_DIR: &str = "/home/user/.apps/os.lazy.webkit";

/// Start `path` through `spawnv` (native or Linux personality) and return the
/// pid or `-errno`; a started child is finished and reaped at once.
fn spawnv(path: &str, linux: bool) -> i64 {
    use crate::process::spawnv::{personality, REQ_WORDS};
    let argv = c(path);
    let mut words = [0u64; REQ_WORDS];
    words[..5].copy_from_slice(&[
        path.as_ptr() as u64,
        path.len() as u64,
        argv.as_ptr() as u64,
        argv.len() as u64,
        1,
    ]);
    words[8] = if linux {
        personality::LINUX
    } else {
        personality::NATIVE
    };
    let code = process::dispatch_for_test(SYS_SPAWNV, words.as_ptr() as u64, 0, 0) as i64;
    if code > 0 {
        task::harness::finish(code as usize, 0);
        let _ = task::reap_child_slot(code as usize);
    }
    code
}

/// A confined label creates no process, by any call; the plain user's own
/// are unaffected, and a refusal leaves the task table as it was.
pub fn no_spawn() -> Result<(), String> {
    with_world(|| {
        // Control: the same calls as the plain user start a process.
        as_plain_user();
        for linux in [false, true] {
            let got = spawnv("/system/bin/helper", linux);
            check!(
                got > 0,
                "the plain user's spawnv (linux={linux}) gave {got}"
            );
        }
        for (nr, a1) in [(SYS_FORK, 0), (SYS_VFORK, 0), (SYS_CLONE, SIGCHLD)] {
            let got = process::linux::dispatch_for_test(nr, a1, 0, 0) as i64;
            check!(got > 0, "the plain user's call {nr} gave {got}");
            task::harness::finish(got as usize, 0);
            let _ = task::reap_child_slot(got as usize);
        }

        confine_as_user(RULES, &[])?;
        let slots = task::free_slots();
        for linux in [false, true] {
            let got = spawnv("/system/bin/helper", linux);
            check!(got == EPERM, "spawnv (linux={linux}) gave {got}");
        }
        for (nr, a1) in [
            (SYS_FORK, 0),
            (SYS_VFORK, 0),
            // clone(SIGCHLD): fork spelled as clone
            (SYS_CLONE, SIGCHLD),
            // clone(CLONE_VM | SIGCHLD): the vfork-style child `posix_spawn` uses
            (SYS_CLONE, CLONE_VM | SIGCHLD),
        ] {
            let got = process::linux::dispatch_for_test(nr, a1, 0, 0) as i64;
            check!(got == EPERM, "call {nr}({a1:#x}) gave {got}");
        }
        check!(
            task::free_slots() == slots,
            "a refused spawn used a task slot: {slots} -> {}",
            task::free_slots()
        );
        Ok(())
    })
}

/// A confined task signals only its own process: not a peer of the same
/// user, not a group, not everyone. The uid rule alone would allow all three.
pub fn signals() -> Result<(), String> {
    with_world(|| {
        signal::harness::reset();
        let label = confine_as_user(RULES, &[])?;
        let user_plain = Cred::new(USER_CRED.0, USER_CRED.1, 0, 0, 0);
        let user_confined = Cred::new(USER_CRED.0, USER_CRED.1, 0, label, 0);

        let attacker = task::spawn_fork().map_err(|e| format!("spawn: {e}"))?;
        let peer = task::spawn_fork().map_err(|e| format!("spawn: {e}"))?;
        credentials::set(attacker, user_confined);
        credentials::set(peer, user_plain);
        let info = SigInfo::user(attacker, signal::SI_USER);

        for sig in [signal::SIGKILL, signal::SIGTERM, signal::SIGSTOP, 0] {
            check!(
                signal::kill(attacker, peer as i64, sig, info) == Err(SignalError::NotPermitted),
                "kill({sig}) of a same-uid peer was not refused"
            );
            check!(
                signal::send_tid(attacker, peer, sig, info) == Err(SignalError::NotPermitted),
                "tkill({sig}) of a same-uid peer was not refused"
            );
        }
        // Broadcast and group forms reach only what the rules allow: nothing.
        check!(
            signal::kill(attacker, -1, signal::SIGTERM, info).is_err(),
            "kill(-1) reached somebody"
        );
        check!(
            task::harness::state(peer) == Some(TaskState::Runnable),
            "a refused signal landed: {:?}",
            task::harness::state(peer)
        );

        // Itself, and its own threads (the same address space).
        check!(
            signal::kill(attacker, attacker as i64, 0, info).is_ok(),
            "a confined task may not probe itself"
        );
        task::harness::switch_current(attacker);
        let thread = task::spawn_thread("sandbox-thread", process::USER_STACK_TOP, 0, 0)
            .map_err(|e| format!("thread: {e}"))?;
        task::harness::switch_current(task::KERNEL_TASK);
        check!(
            signal::send_tid(attacker, thread, 0, info).is_ok(),
            "a confined task may not signal its own thread"
        );

        // The same sender without the label may signal its peer (control).
        credentials::set(attacker, user_plain);
        check!(
            signal::kill(attacker, peer as i64, 0, info).is_ok(),
            "the plain user could not signal a same-uid peer"
        );

        while task::reap_child().is_some() {}
        signal::harness::reset();
        Ok(())
    })
}

/// An image that starts confined keeps its standard streams and connected
/// sockets and nothing else a launcher left open: files, directories, pipes,
/// eventfds, memory objects and listeners are closed, for a confined label
/// only.
pub fn scrub_inherited() -> Result<(), String> {
    with_world(|| {
        crate::ipc::unix::clear_for_test();
        as_plain_user();
        let stdio: Vec<task::FdKind> = (0..3).map(task::fd_kind).collect();

        // Everything a launcher might have left open.
        let file = openat(
            "/system/share/fonts/liberation/LiberationSans-Regular.ttf",
            O_RDONLY,
        );
        let dir = openat("/system/share/fonts", O_RDONLY | O_DIRECTORY);
        let mut pair = [0i32; 2];
        let got = process::linux::dispatch_args_for_test(
            53,
            AF_UNIX,
            SOCK_STREAM,
            0,
            pair.as_mut_ptr() as u64,
        ) as i64;
        check!(got == 0, "socketpair gave {got}");
        let mut pipe = [0i32; 2];
        check!(
            process::linux::dispatch_for_test(22, pipe.as_mut_ptr() as u64, 0, 0) as i64 == 0,
            "pipe"
        );
        let event = process::linux::dispatch_for_test(290, 0, 0, 0) as i64;
        let name = c("scrub");
        let memfd = process::linux::dispatch_for_test(319, name.as_ptr() as u64, 0, 0) as i64;
        let listener = socket(AF_UNIX, SOCK_STREAM);
        check!(bind(listener, b"/tmp/scrub.sock") == 0, "bind");
        let leftovers = [
            file,
            dir,
            i64::from(pipe[0]),
            i64::from(pipe[1]),
            event,
            memfd,
            listener,
        ];
        for fd in leftovers
            .iter()
            .chain(&[i64::from(pair[0]), i64::from(pair[1])])
        {
            check!(*fd >= 3, "a descriptor was not made: {fd}");
        }

        // An unconfined image keeps all of it.
        check!(
            confine::scrub_inherited_descriptors() == 0,
            "an unconfined task lost descriptors"
        );
        for fd in leftovers {
            check!(
                task::fd_kind(fd as usize) != task::FdKind::Closed,
                "descriptor {fd} closed for an unconfined task"
            );
        }

        // A confined one is left with the streams and the connected socket.
        confine_as_user(RULES, &[])?;
        let closed = confine::scrub_inherited_descriptors();
        check!(
            closed == leftovers.len(),
            "closed {closed}, want {}",
            leftovers.len()
        );
        for fd in leftovers {
            check!(
                task::fd_kind(fd as usize) == task::FdKind::Closed,
                "descriptor {fd} survived the scrub"
            );
        }
        for fd in pair {
            check!(
                task::fd_kind(fd as usize) == task::FdKind::Socket,
                "the connected socket {fd} was closed"
            );
        }
        for (fd, kind) in stdio.iter().enumerate() {
            check!(task::fd_kind(fd) == *kind, "standard stream {fd} changed");
        }
        for fd in pair {
            close(i64::from(fd));
        }
        crate::ipc::unix::clear_for_test();
        Ok(())
    })
}

/// The calls that would let a sandbox read or steer other processes, change
/// its root, or mount, are not implemented; a label rule can make none of
/// them reachable. If one is ever added it must be gated and this test says
/// so.
pub fn enosys() -> Result<(), String> {
    with_world(|| {
        confine_as_user(RULES, &[])?;
        for (nr, name) in [
            (101u64, "ptrace"),
            (310, "process_vm_readv"),
            (311, "process_vm_writev"),
            (165, "mount"),
            (166, "umount2"),
            (161, "chroot"),
            (272, "unshare"),
            (308, "setns"),
            (322, "execveat"),
            (435, "clone3"),
            (425, "io_uring_setup"),
            (323, "userfaultfd"),
            (312, "kcmp"),
        ] {
            let got = process::linux::dispatch_for_test(nr, 0, 0, 0) as i64;
            check!(got == ENOSYS, "{name} ({nr}) gave {got}, not ENOSYS");
        }
        Ok(())
    })
}
