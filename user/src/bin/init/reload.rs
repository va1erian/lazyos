//! Service hot reload (docs/dbgd-plan.md, v2; issue #701): `dbgd` hands
//! `init` a rebuilt service binary, `init` runs it in place of the image's
//! and rolls back on its own if the new run does not hold.
//!
//! The rollback lives here, not in `dbgd`, because the reloaded service may
//! be the one carrying `dbgd`'s connection (`netdrv`, `netd`): nobody may be
//! left to ask for it. A reload is held to a trial: an exit or a failed
//! spawn before the trial ends puts the image's binary back and restarts the
//! row at once; a run still alive at the deadline is committed. Nothing is
//! written to the OS volume: the binary sits on the ramfs, so a reboot
//! always comes back to the image.
//!
//! Accepted from `dbgd`'s kernel-stamped identity alone, and only when this
//! box's `/boot/lazyos.cfg` says `diag.dbg.control=1` (read here, not taken
//! from the requester). Serial lines: `INIT:RELOAD:TRIAL|COMMIT|ROLLBACK|
//! REVERT|RESTART|DENIED service=...`.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use core::sync::atomic::{AtomicBool, Ordering};

use dbgwire::control;
use user::messenger::{self, errno, services, Error, Message, Parcel};
use user::sys;

use super::service::{Phase, Service};

/// Ticks per second of `sys::clock`.
const TICKS_PER_SECOND: u64 = 100;
/// Bytes of `lazyos.cfg` read (the kernel caps the file at 4 KiB).
#[cfg(lazyos_dbgd)]
const CFG_LIMIT: usize = 4096;

/// A row's hot-reload state.
#[derive(Default)]
pub(super) struct Hot {
    /// The binary run in place of the image's, while a reload holds.
    binary: Option<String>,
    /// Its SHA-256 (hex).
    sha256: String,
    /// The trial length (ticks) the next spawn starts.
    trial_ticks: Option<u64>,
    /// The tick the current trial ends at.
    trial_until: Option<u64>,
    /// The next exit is the kill a reload or restart asked for.
    killed_for_reload: bool,
    /// The last state `Reloads` shows; `None` for a row never reloaded.
    state: Option<&'static str>,
    /// Why the last rollback happened.
    detail: String,
}

/// Whether [`prepare`] made the reload directory, root's and 0755.
static DIR_READY: AtomicBool = AtomicBool::new(false);

/// Make the directory reloaded binaries are copied to, once, before `init`
/// spawns anything. `/transient` is world-writable (sticky): made later, a
/// task could have created the name first and own what `init` then runs as
/// root. At this point `init` is the only task and the ramfs is fresh, so a
/// `mkdir` that succeeds makes a directory only root owns, and the sticky
/// bit keeps anyone else from removing or renaming it. If it fails, reloads
/// are refused for this boot.
pub(super) fn prepare() {
    let made = user::files::mkdir(fhs::state::INIT_RELOAD).is_ok()
        && user::files::chmod(fhs::state::INIT_RELOAD, 0o755).is_ok();
    DIR_READY.store(made, Ordering::Relaxed);
}

/// The program a spawn of `service` runs: the reloaded binary, if any.
pub(super) fn program(service: &Service) -> &str {
    service.hot.binary.as_deref().unwrap_or(service.path)
}

/// Whether `message` comes from `dbgd` on a box that allows control.
#[cfg(lazyos_dbgd)]
pub(super) fn authorized(message: &Message) -> bool {
    let caller = message.caller();
    let dbgd =
        caller.uid == dbgwire::config::DBGD_UID && caller.label_id == 0 && caller.session == 0;
    dbgd && control_allowed()
}

#[cfg(not(lazyos_dbgd))]
pub(super) fn authorized(_message: &Message) -> bool {
    false
}

/// `diag.dbg.control=1` in `lazyos.cfg`, read once: the boot volume does
/// not change under a running system.
#[cfg(lazyos_dbgd)]
fn control_allowed() -> bool {
    use core::sync::atomic::{AtomicU8, Ordering};
    static KNOWN: AtomicU8 = AtomicU8::new(0);
    match KNOWN.load(Ordering::Relaxed) {
        1 => false,
        2 => true,
        _ => {
            let on = user::files::read_up_to(fhs::boot::LAZYOS_CFG_PATH, CFG_LIMIT)
                .ok()
                .and_then(|bytes| String::from_utf8(bytes).ok())
                .is_some_and(|cfg| dbgwire::config::control_enabled(&cfg));
            KNOWN.store(if on { 2 } else { 1 }, Ordering::Relaxed);
            on
        }
    }
}

fn denied(name: &str, why: &str) -> Error {
    sys::write_str(&format!(
        "INIT:RELOAD:DENIED service={} reason=\"{why}\"\n",
        elevpolicy::audit::token(name)
    ));
    Error::Errno(-errno::EPERM)
}

/// The manifest row `name` may be reloaded now: its index.
fn target(services: &[Service], message: &Message, name: &str) -> messenger::Result<usize> {
    if !authorized(message) {
        return Err(denied(name, "only dbgd, with diag.dbg.control=1"));
    }
    if let Err(why) = control::reloadable(name) {
        return Err(denied(name, why.text()));
    }
    if super::shutdown::stopping() {
        return Err(Error::Errno(-errno::EAGAIN));
    }
    let index = services
        .iter()
        .position(|row| !row.launched && row.name == name)
        .ok_or(Error::Errno(-errno::ENOENT))?;
    if matches!(services[index].phase, Phase::Pending | Phase::Stopping) {
        return Err(Error::Errno(-errno::EAGAIN));
    }
    Ok(index)
}

/// Copy `dbgd`'s staged file for `name` where only root writes, after
/// checking it is the binary the client hashed. Returns the copy's path.
fn install(name: &str, staged: &str, sha256: &str) -> messenger::Result<String> {
    let invalid = |why: &str| {
        sys::write_str(&format!(
            "INIT:RELOAD:FAIL service={name} reason=\"{why}\"\n"
        ));
        Error::Errno(-errno::EINVAL)
    };
    if !control::is_staged_path(name, staged) {
        return Err(invalid("not dbgd's staging file for this service"));
    }
    let want = control::parse_digest(sha256).map_err(|why| invalid(why.text()))?;
    let mut data = Vec::new();
    user::files::read_large(staged, control::MAX_BINARY as usize, &mut data)
        .map_err(|_| invalid("cannot read the staged binary"))?;
    if lazyos_crypto::sha256::sha256(&data) != want {
        return Err(invalid("sha256 does not match the staged binary"));
    }
    if !data.starts_with(b"\x7fELF") {
        return Err(invalid("not an ELF file"));
    }
    if !DIR_READY.load(Ordering::Relaxed) {
        return Err(invalid("the reload directory was not made at boot"));
    }
    let path = control::reload_path(name);
    user::files::write_large(&path, &data).map_err(|_| invalid("cannot write the copy"))?;
    let _ = user::files::chmod(&path, 0o755);
    Ok(path)
}

/// Stop the row's current run so the supervisor starts it again at once:
/// kill a running task (its exit is ours, [`exited`]), or schedule a row
/// that is down. Returns the killed pid, or 0.
fn restart_now(row: &mut Service) -> messenger::Result<u64> {
    if row.phase == Phase::Running && row.pid != 0 {
        let pid = row.pid;
        sys::kill(pid, sys::SIG_KILL).map_err(Error::Errno)?;
        row.hot.killed_for_reload = true;
        return Ok(pid);
    }
    row.phase = Phase::Restarting;
    row.next_start = sys::clock();
    row.restarts = 0;
    Ok(0)
}

fn reply(method: u32, body: Vec<u8>) -> Parcel {
    Parcel {
        header: services::header(services::init::INTERFACE, method),
        body,
        ..Parcel::default()
    }
}

/// `ReloadService`.
pub(super) fn reload(services: &mut [Service], message: &Message) -> messenger::Result<Parcel> {
    let args = services::init::wire::decode_reload_service_args(&message.parcel.body)
        .map_err(Error::Parcel)?;
    let index = target(services, message, &args.name)?;
    let row = &mut services[index];
    if args.binary.is_empty() {
        sys::write_str(&format!("INIT:RELOAD:RESTART service={}\n", row.name));
    } else {
        let path = install(row.name, &args.binary, &args.sha256)?;
        let ms = u64::from(args.trial_ms).clamp(control::TRIAL_MIN_MS, control::TRIAL_MAX_MS);
        row.hot.binary = Some(path);
        row.hot.sha256 = args.sha256.to_ascii_lowercase();
        row.hot.trial_ticks = Some(ms * TICKS_PER_SECOND / 1000);
        row.hot.trial_until = None;
        row.hot.state = Some("trial");
        row.hot.detail.clear();
    }
    let pid = restart_now(row)?;
    let body = services::init::wire::encode_reload_service_reply(
        &services::init::wire::ReloadServiceReply { pid },
    )
    .map_err(Error::Parcel)?;
    Ok(reply(services::init::wire::METHOD_RELOADSERVICE, body))
}

/// `RevertService`.
pub(super) fn revert(services: &mut [Service], message: &Message) -> messenger::Result<Parcel> {
    let args = services::init::wire::decode_revert_service_args(&message.parcel.body)
        .map_err(Error::Parcel)?;
    let index = target(services, message, &args.name)?;
    let row = &mut services[index];
    if row.hot.binary.is_some() {
        drop_binary(row, "reverted", "");
        sys::write_str(&format!("INIT:RELOAD:REVERT service={}\n", row.name));
    }
    let pid = restart_now(row)?;
    let body = services::init::wire::encode_revert_service_reply(
        &services::init::wire::RevertServiceReply { pid },
    )
    .map_err(Error::Parcel)?;
    Ok(reply(services::init::wire::METHOD_REVERTSERVICE, body))
}

/// `Reloads`.
pub(super) fn list(services: &[Service]) -> messenger::Result<Parcel> {
    let reloads = services
        .iter()
        .filter_map(|row| {
            Some(services::init::wire::ReloadState {
                name: String::from(row.name),
                state: String::from(row.hot.state?),
                sha256: row.hot.sha256.clone(),
                pid: row.pid,
                detail: row.hot.detail.clone(),
            })
        })
        .collect();
    let body =
        services::init::wire::encode_reloads_reply(&services::init::wire::ReloadsReply { reloads })
            .map_err(Error::Parcel)?;
    Ok(reply(services::init::wire::METHOD_RELOADS, body))
}

/// Back to the image's binary.
fn drop_binary(row: &mut Service, state: &'static str, detail: &str) {
    if let Some(path) = row.hot.binary.take() {
        let _ = user::files::remove(&path);
    }
    row.hot.sha256.clear();
    row.hot.trial_ticks = None;
    row.hot.trial_until = None;
    row.hot.state = Some(state);
    row.hot.detail = String::from(detail);
}

fn rollback(row: &mut Service, why: &str) {
    sys::write_str(&format!(
        "INIT:RELOAD:ROLLBACK service={} reason=\"{why}\"\n",
        row.name
    ));
    drop_binary(row, "rolled-back", why);
}

/// A spawn of `row` succeeded: a pending trial starts now.
pub(super) fn spawned(row: &mut Service) {
    if let Some(ticks) = row.hot.trial_ticks.take() {
        row.hot.trial_until = Some(row.started_tick + ticks);
        sys::write_str(&format!(
            "INIT:RELOAD:TRIAL service={} pid={} sha256={} ticks={ticks}\n",
            row.name, row.pid, row.hot.sha256
        ));
    }
}

/// A spawn of `row` failed: `true` when it was the reloaded binary, which
/// is rolled back (the caller retries the image's at once).
pub(super) fn spawn_failed(row: &mut Service) -> bool {
    if row.hot.binary.is_none() {
        return false;
    }
    rollback(row, "spawn failed");
    true
}

/// `row`'s task exited: `true` when the exit is the reload's own kill or a
/// failed trial (rolled back), so the caller restarts the row at once
/// instead of applying its restart policy.
pub(super) fn exited(row: &mut Service, status: u64) -> bool {
    if core::mem::take(&mut row.hot.killed_for_reload) {
        return true;
    }
    if row.hot.trial_until.is_some() {
        rollback(row, &format!("exited status={status} during the trial"));
        return true;
    }
    false
}

/// Commit every trial whose run is still alive at its deadline.
pub(super) fn sweep(services: &mut [Service], now: u64) {
    for row in services.iter_mut() {
        let due = row.hot.trial_until.is_some_and(|until| until <= now);
        if due && row.phase == Phase::Running {
            row.hot.trial_until = None;
            row.hot.state = Some("committed");
            sys::write_str(&format!(
                "INIT:RELOAD:COMMIT service={} pid={} sha256={}\n",
                row.name, row.pid, row.hot.sha256
            ));
        }
    }
}

/// The earliest trial deadline, for the supervisor's wake.
pub(super) fn next_deadline(services: &[Service]) -> Option<u64> {
    services.iter().filter_map(|row| row.hot.trial_until).min()
}
