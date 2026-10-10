//! App swapping (docs/dbgd-plan.md, v2): after `pkgd.InstallDebug` put a
//! new build of an app in place, `dbgd` asks `init` to restart what runs of
//! it. Every running instance is killed and launched again through the
//! ordinary `Launch` path, in its own session and with the argument it was
//! launched with, so it comes back from the app's current install with its
//! label, policy and session environment, as a fresh launch would.
//!
//! Same gate as a service reload (`reload.rs`): `dbgd`'s identity and the
//! box's `diag.dbg.control=1`. Serial lines: `INIT:RELAUNCH:PASS|FAIL`.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use user::messenger::{self, errno, router, services, Error, Message, Parcel};
use user::sys::{self, Cred as SysCred, CAP_SETUID};

use super::installed::InstalledApps;
use super::launch::launch;
use super::service::{Phase, Service};

/// The identity a relaunch is made with: `init` itself, which may launch
/// into any session (`launch::authorize`); the instance gets its session's
/// credentials, never this one.
const SUPERVISOR: SysCred = SysCred::new(0, 0, CAP_SETUID, 0, 0);

/// `RelaunchApp`.
pub(super) fn relaunch(
    services: &mut Vec<Service>,
    broker: &mut router::TopicBroker,
    installed: &mut InstalledApps,
    message: &Message,
) -> messenger::Result<Parcel> {
    let args = services::init::wire::decode_relaunch_app_args(&message.parcel.body)
        .map_err(Error::Parcel)?;
    if !super::reload::authorized(message) || !dbgwire::control::valid_app_id(&args.app) {
        sys::write_str(&format!(
            "INIT:RELAUNCH:DENIED app={}\n",
            elevpolicy::audit::token(&args.app)
        ));
        return Err(Error::Errno(-errno::EPERM));
    }
    if super::shutdown::stopping() {
        return Err(Error::Errno(-errno::EAGAIN));
    }
    // A core app's short alias names the same rows as its `system_name`.
    installed.refresh();
    let id = installed
        .find(&args.app)
        .map_or(args.app.as_str(), |app| app.id);
    let again = stop_instances(services, id);
    let stopped = again.len() as u64;
    let mut started = 0;
    for (session, arg) in again {
        let request = services::init::wire::LaunchArgs {
            app: String::from(id),
            args: arg.unwrap_or_default(),
            session,
        };
        match launch(services, broker, installed, &request, &SUPERVISOR, false) {
            Ok(result) => {
                started += 1;
                sys::write_str(&format!(
                    "INIT:RELAUNCH:PASS app={id} pid={} session={session}\n",
                    result.pid
                ));
            }
            Err(error) => sys::write_str(&format!(
                "INIT:RELAUNCH:FAIL app={id} session={session} errno={}\n",
                error.errno().map_or(0, |e| -e)
            )),
        }
    }
    let body =
        services::init::wire::encode_relaunch_app_reply(&services::init::wire::RelaunchAppReply {
            stopped,
            started,
        })
        .map_err(Error::Parcel)?;
    Ok(Parcel {
        header: services::header(
            services::init::INTERFACE,
            services::init::wire::METHOD_RELAUNCHAPP,
        ),
        body,
        ..Parcel::default()
    })
}

/// Kill every running instance of `id`, returning where each ran and with
/// what argument. The rows are left `Stopping`: their exit retires them
/// without a restart, and a resident app's next launch does not find them.
fn stop_instances(services: &mut [Service], id: &str) -> Vec<(u64, Option<String>)> {
    let mut again = Vec::new();
    for row in services.iter_mut() {
        let running = row.launched && row.name == id && row.phase == Phase::Running && row.pid != 0;
        if !running || sys::kill(row.pid, sys::SIG_KILL).is_err() {
            continue;
        }
        row.phase = Phase::Stopping;
        row.killed = true;
        row.stop_deadline = sys::clock();
        again.push((
            row.cred.map_or(0, |cred| cred.session),
            row.launch_arg.clone(),
        ));
    }
    again
}
