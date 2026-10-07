//! The services LazyShell is a client of: `init` (app registry, `Launch`,
//! `Shutdown`), `logind` (`Logout`), `confd` (`sys/ui/*`) and `timed` (the
//! zone name).
//!
//! Every call is bounded, and an unregistered service is not waited for
//! (`Service::try_connect`): the shell must keep painting while a service is
//! late, and simply tries again on a later tick. Bodies come from the
//! generated stubs; resolved endpoints stay cached for the task's life (closing
//! one would hang up the service, see `platform::messenger`).

use confd::Value;
use messenger_generated::os_lazy_confd_v1 as confd_wire;
use messenger_generated::os_lazy_init_v1 as init_wire;
use messenger_generated::os_lazy_logind_v1 as logind_wire;
use messenger_generated::os_lazy_timed_v1 as timed_wire;

use crate::platform::confd_store::from_wire;
use crate::platform::messenger::Service;
use crate::server::ERROR_FIELD;
use crate::sys::{self, errno};

/// `init`'s registered name (owner of the app registry).
const INIT: &str = "os.lazy.init";
/// `confd`'s registered name.
const CONFD: &str = "os.lazy.confd";
/// `timed`'s registered name.
const TIMED: &str = "os.lazy.timed";
/// `logind`'s registered name.
const LOGIND: &str = "os.lazy.logind";

/// `ListApps` may take a quarter second (the old compositor menu's bound).
const LIST_TICKS: u64 = 25;
/// `Services` (a cached reply in `init`): a tenth of a second.
const SERVICES_TICKS: u64 = 10;
/// `Launch` replies after the spawn, which takes a moment under emulation.
const LAUNCH_TICKS: u64 = 1000;
/// `Shutdown`: `init` answers before it stops anything, so this is a
/// backstop (the compositor menu's bound in #504).
const SHUTDOWN_TICKS: u64 = 300;
/// One `confd` read.
const CONFD_TICKS: u64 = 50;
/// One `timed` read.
const TIMED_TICKS: u64 = 25;

/// One `init` registry row the shell uses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct App {
    pub id: String,
    pub name: String,
    pub installed: bool,
    /// The package's menu category (empty for a built-in).
    pub category: String,
    /// Left out of this user's menu and desktop (`init` resolves the
    /// user-over-machine keys for the caller).
    pub hidden: bool,
    /// The package's 32-pixel icon path (empty for a built-in).
    pub icon: String,
}

impl App {
    /// The row as the start-menu model reads it.
    pub fn listed(&self) -> lazyshell::menu::Listed<'_> {
        lazyshell::menu::Listed {
            id: &self.id,
            name: &self.name,
            installed: self.installed,
            category: &self.category,
            hidden: self.hidden,
        }
    }
}

/// A bounded call on the service registered as `name`.
fn call(
    name: &'static str,
    interface: u64,
    method: u32,
    body: Vec<u8>,
    ticks: u64,
) -> Result<libmessenger::Parcel, i64> {
    let service = Service::try_connect(name).ok_or(-errno::ENOENT)?;
    let deadline = sys::clock_ticks().saturating_add(ticks);
    service.call_until(interface, method, ERROR_FIELD, body, deadline)
}

/// `init.ListApps`: the shipped and installed apps.
pub fn list_apps() -> Result<Vec<App>, i64> {
    let reply = call(
        INIT,
        init_wire::INTERFACE_ID,
        init_wire::METHOD_LISTAPPS,
        Vec::new(),
        LIST_TICKS,
    )?;
    let list = init_wire::decode_list_apps_reply(&reply.body).map_err(|_| -errno::EINVAL)?;
    Ok(list
        .apps
        .into_iter()
        .map(|app| App {
            id: app.id,
            name: app.name,
            installed: app.installed,
            category: app.category,
            hidden: app.hidden,
            icon: app.icon,
        })
        .collect())
}

/// `init.Services`: every supervision row as `(name, pid)`, which maps a
/// launched app's task to its app id (the tray's caller identity). `init`
/// answers it from a cached reply, so the bound is short: the tray reads it
/// on the UI thread.
pub fn launched() -> Result<Vec<(String, u64)>, i64> {
    let reply = call(
        INIT,
        init_wire::INTERFACE_ID,
        init_wire::METHOD_SERVICES,
        Vec::new(),
        SERVICES_TICKS,
    )?;
    let table = init_wire::decode_services_reply(&reply.body).map_err(|_| -errno::EINVAL)?;
    Ok(table
        .services
        .into_iter()
        .map(|row| (row.name, row.pid))
        .collect())
}

/// `init.Launch(app, arg, 0)`: start `app` in the shell's own session, with
/// `arg` (one absolute path, or `""`); the new task's pid.
pub fn launch(app: &str, arg: &str) -> Result<u64, i64> {
    let body = init_wire::encode_launch_args(&init_wire::LaunchArgs {
        app: app.to_owned(),
        args: arg.to_owned(),
        session: 0,
    })
    .map_err(|_| -errno::EINVAL)?;
    let reply = call(
        INIT,
        init_wire::INTERFACE_ID,
        init_wire::METHOD_LAUNCH,
        body,
        LAUNCH_TICKS,
    )?;
    init_wire::decode_launch_reply(&reply.body)
        .map(|reply| reply.pid)
        .map_err(|_| -errno::EINVAL)
}

/// `init.Shutdown(mode, reason, force = false)`: begin an orderly stop
/// (docs/shutdown.md); the phase `init` reports.
pub fn shutdown(mode: u32, reason: &str) -> Result<String, i64> {
    let body = init_wire::encode_shutdown_args(&init_wire::ShutdownArgs {
        mode,
        reason: reason.to_owned(),
        force: false,
    })
    .map_err(|_| -errno::EINVAL)?;
    let reply = call(
        INIT,
        init_wire::INTERFACE_ID,
        init_wire::METHOD_SHUTDOWN,
        body,
        SHUTDOWN_TICKS,
    )?;
    init_wire::decode_shutdown_reply(&reply.body)
        .map(|reply| reply.phase)
        .map_err(|_| -errno::EINVAL)
}

/// `logind.Logout()` (issue #623): end the shell's own desktop session.
/// `logind` answers before anything stops; `init` then ends every task of
/// the session, this shell included, and the login screen comes back.
pub fn logout() -> Result<u64, i64> {
    let reply = call(
        LOGIND,
        logind_wire::INTERFACE_ID,
        logind_wire::METHOD_LOGOUT,
        Vec::new(),
        SHUTDOWN_TICKS,
    )?;
    logind_wire::decode_logout_reply(&reply.body)
        .map(|reply| reply.session)
        .map_err(|_| -errno::EINVAL)
}

/// `confd.Get(key)`: `Ok(None)` when the key is absent.
pub fn confd_get(key: &str) -> Result<Option<Value>, i64> {
    let body = confd_wire::encode_get_args(&confd_wire::GetArgs {
        path: key.to_owned(),
    })
    .map_err(|_| -errno::EINVAL)?;
    match call(
        CONFD,
        confd_wire::INTERFACE_ID,
        confd_wire::METHOD_GET,
        body,
        CONFD_TICKS,
    ) {
        Ok(reply) => {
            let decoded = confd_wire::decode_get_reply(&reply.body).map_err(|_| -errno::EINVAL)?;
            Ok(decoded.value.as_ref().and_then(from_wire))
        }
        // confd answers an absent key with its own NOT_FOUND code; any
        // structured refusal means "no usable value" to the shell, while a
        // transport failure (no confd, timeout) is an error to retry.
        Err(code) if is_transport(code) => Err(code),
        Err(_) => Ok(None),
    }
}

/// `timed.GetZone`: the configured zone name.
pub fn zone() -> Result<String, i64> {
    let reply = call(
        TIMED,
        timed_wire::INTERFACE_ID,
        timed_wire::METHOD_GETZONE,
        Vec::new(),
        TIMED_TICKS,
    )?;
    timed_wire::decode_get_zone_reply(&reply.body)
        .map(|reply| reply.name)
        .map_err(|_| -errno::EINVAL)
}

/// Whether `code` means the service could not be reached (rather than that it
/// answered with a refusal).
fn is_transport(code: i64) -> bool {
    [errno::ENOENT, errno::EPIPE, errno::ETIMEDOUT]
        .iter()
        .any(|known| code == -known)
}
