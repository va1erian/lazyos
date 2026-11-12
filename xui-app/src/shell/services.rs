//! The services LazyShell is a client of: `init` (app registry, `Launch`,
//! `Shutdown`), `logind` (`Logout`), `confd` (`sys/ui/*`) and `timed` (the
//! zone name).
//!
//! Every call is bounded, and an unregistered service is not waited for
//! (`Client::try_connect`): the shell must keep painting while a service is
//! late, and simply tries again on a later tick. The generated clients stay
//! cached for the task's life (closing one would hang up the service, see
//! `platform::messenger`); the bounds are the `.midl` deadlines, except where
//! the shell needs a shorter one (`_within`).

use std::rc::Rc;

use confd::Value;
use libmessenger::client::{CallError, Deadline};
use messenger_generated::os_lazy_confd_v1 as confd_wire;
use messenger_generated::os_lazy_init_v1 as init_wire;
use messenger_generated::os_lazy_logind_v1 as logind_wire;
use messenger_generated::os_lazy_timed_v1 as timed_wire;

use crate::platform::confd_store::from_wire;
use crate::platform::messenger::{client, Observed};
use crate::sys::errno;

/// `ListApps` may take a quarter second (the old compositor menu's bound).
const LIST_TICKS: u64 = 25;
/// `Services` (a cached reply in `init`): a tenth of a second.
const SERVICES_TICKS: u64 = 10;
/// One `confd` read.
const CONFD_TICKS: u64 = 50;

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

/// The generated client of a service, which is not waited for: `-ENOENT` while
/// it is not registered.
fn service<C: 'static, E>(connect: fn(Observed) -> Result<C, CallError<E>>) -> Result<Rc<C>, i64> {
    client(connect).map_err(|_| -errno::ENOENT)
}

/// `init.ListApps`: the shipped and installed apps.
pub fn list_apps() -> Result<Vec<App>, i64> {
    let init = service(init_wire::Client::try_connect)?;
    let apps = init.list_apps_within(Deadline::ticks(LIST_TICKS))?;
    Ok(apps
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

/// `init.Services` as [`launched`] reads it, with the kernel id of the label
/// each running task carries: `(name, pid, label)`. A process an app started
/// keeps the app's label, which is how the notification service names the
/// sender of a call whose task is not the one `init` launched.
pub fn launched_labelled() -> Result<Vec<(String, u64, u32)>, i64> {
    let init = service(init_wire::Client::try_connect)?;
    let table = init.services_within(Deadline::ticks(SERVICES_TICKS))?;
    Ok(table
        .into_iter()
        .map(|row| (row.name, row.pid, row.label))
        .collect())
}

/// `init.Services`: every supervision row as `(name, pid)`, which maps a
/// launched app's task to its app id (the tray's caller identity). `init`
/// answers it from a cached reply, so the bound is short: the tray reads it
/// on the UI thread.
pub fn launched() -> Result<Vec<(String, u64)>, i64> {
    Ok(launched_labelled()?
        .into_iter()
        .map(|(name, pid, _)| (name, pid))
        .collect())
}

/// `init.Stop(app)`: stop every instance of `app` in the shell's session
/// (the tray's Quit row); how many were running. `Stop` answers once every
/// instance is gone (with T3's quit grace, up to about 3 s), so it waits the
/// default bound, more than a spawn.
pub fn stop(app: &str) -> Result<u64, i64> {
    let init = service(init_wire::Client::try_connect)?;
    Ok(init.stop(app)?)
}

/// `init.Launch(app, arg, 0)`: start `app` in the shell's own session, with
/// `arg` (one absolute path, or `""`); the task's pid, and whether it is a
/// resident app's running instance that got the launch as `Reopen`.
pub fn launch(app: &str, arg: &str) -> Result<(u64, bool), i64> {
    let init = service(init_wire::Client::try_connect)?;
    let reply = init.launch(app, arg, 0)?;
    Ok((reply.pid, reply.existing))
}

/// `init.Shutdown(mode, reason, force = false)`: begin an orderly stop
/// (docs/shutdown.md); the phase `init` reports.
pub fn shutdown(mode: u32, reason: &str) -> Result<String, i64> {
    let init = service(init_wire::Client::try_connect)?;
    Ok(init.shutdown(mode, reason, false)?.phase)
}

/// `logind.Logout()` (issue #623): end the shell's own desktop session.
/// `logind` answers before anything stops; `init` then ends every task of
/// the session, this shell included, and the login screen comes back.
pub fn logout() -> Result<u64, i64> {
    let logind = service(logind_wire::Client::try_connect)?;
    Ok(logind.logout()?)
}

/// `confd.Get(key)`: `Ok(None)` when the key is absent.
pub fn confd_get(key: &str) -> Result<Option<Value>, i64> {
    let confd = service(confd_wire::Client::try_connect)?;
    match confd.get_within(key, Deadline::ticks(CONFD_TICKS)) {
        Ok(value) => Ok(value.as_ref().and_then(from_wire)),
        // confd answers an absent key with its own NOT_FOUND code; any
        // structured refusal means "no usable value" to the shell, while a
        // transport failure (no confd, timeout) is an error to retry.
        Err(CallError::Transport(failed)) if is_transport(failed.to_syscall()) => {
            Err(failed.to_syscall())
        }
        Err(_) => Ok(None),
    }
}

/// `timed.GetZone`: the configured zone name.
pub fn zone() -> Result<String, i64> {
    let timed = service(timed_wire::Client::try_connect)?;
    Ok(timed.get_zone()?)
}

/// Whether `code` means the service could not be reached (rather than that it
/// answered with a refusal).
fn is_transport(code: i64) -> bool {
    [errno::ENOENT, errno::EPIPE, errno::ETIMEDOUT]
        .iter()
        .any(|known| code == -known)
}
