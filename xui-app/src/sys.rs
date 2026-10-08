//! The native syscalls this crate uses, from `lazyos-sys` (issue #666), the
//! one crate that issues `int 0x80` for native and static-musl programs
//! alike. `int 0x80` is dispatched by task, not by binary kind, so a static
//! musl program reaches the same kernel code a native `user` program does.
//!
//! The names here are the ones this crate (and LazyRAD, through
//! `xui_app::sys`) has always called them by: Messenger calls carry a `msg_`
//! prefix, the PIT clock is [`clock_ticks`]. Nothing is defined here but the
//! labelled spawn's one shape.

pub use lazyos_sys::cred::{cred_get, Cred};
pub use lazyos_sys::display::{
    button, display_bind, display_input_poll, display_op as op, display_present, display_unbind,
    event, key, DisplayEvent as RawEvent, DisplayInfo, EVENT_BYTES,
};
pub use lazyos_sys::errno;
pub use lazyos_sys::msg::parcel::{
    call as msg_call, connect as msg_connect, register as msg_register, reply as msg_reply,
    resolve as msg_resolve, send as msg_send,
};
pub use lazyos_sys::msg::{
    buffer_close, buffer_create, buffer_map, close as msg_close, create_pair as msg_create_pair,
    op as msg_op, queued as msg_queued, recv as msg_recv, recv_from as msg_recv_from,
    release as msg_release, wait_any_ns as msg_wait_any_ns, MsgArgs, MsgResult, SenderId,
    EXPIRED_DEADLINE, FD_READY, REGISTRY_TARGET_SELF, WAIT_FD, WAIT_FD_SHIFT, WAIT_MAX_ENDPOINTS,
};
pub use lazyos_sys::time::{clock as clock_ticks, monotonic_ns, wall_centis};

/// Park until one of `handles` has a message (or a closed peer) or the
/// absolute PIT `deadline` passes: the ready mask, or `-ETIMEDOUT`.
pub fn msg_wait_any(handles: &[u64], deadline: u64) -> Result<u64, i64> {
    lazyos_sys::msg::wait_any(handles, 0, deadline)
}

/// Sleep for `millis` milliseconds: `std::thread::sleep`, kept under this
/// name for the many poll loops that call it. (Apps once avoided std's sleep
/// because LazyOS read an absolute `clock_nanosleep` deadline as a duration;
/// the kernel honours `TIMER_ABSTIME` now, and the suite's
/// `linux_clock_nanosleep_*` tests and the `time` ABI fixture check both
/// kinds of wait, issue #669. Unlike the native `sleep_ms`, std resumes a
/// sleep a signal interrupted.)
pub fn sleep_millis(millis: u64) {
    std::thread::sleep(std::time::Duration::from_millis(millis));
}

/// Start the static Linux program at `path` as a child of this task, stamped
/// with `cred` (its `label_id` is ignored) and the label `label`, with
/// `stdio[i]` (this task's descriptor, `None` for the terminal) as its
/// descriptor `i`: how an IDE that is itself a package runs the project it
/// edits under `dev:<system_name>` and still reads its output (issue #529).
/// Returns the child's pid, or the negative errno (`-EACCES` when the kernel
/// refuses the label, `-EBADF` for a closed descriptor).
pub fn spawn_labelled(
    path: &str,
    argv: &[&str],
    envp: &[&str],
    cred: Cred,
    label: &str,
    stdio: [Option<i32>; 3],
) -> Result<u64, i64> {
    use lazyos_sys::spawn::{spawnv_stdio, Personality, SpawnCred};
    spawnv_stdio(
        path,
        argv,
        envp,
        Personality::Linux,
        SpawnCred::AsLabelled(cred, label),
        Some(stdio),
    )
}
