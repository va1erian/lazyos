//! `init`'s requests: the generated servers of `os.lazy.init.v1` and the
//! app lifecycle line `os.lazy.init.app.v1` over the supervisor's tables.
//!
//! A `Stop` and a `Home` answer later (when their targets exit or their
//! helper does): the method records the transaction with the table that
//! answers it, sets [`Handler::held`] and returns a refusal the loop drops.

use alloc::string::String;
use alloc::vec::Vec;
use libmessenger::server::{Caller, Fault};
use messenger_generated::os_lazy_init_app_v1 as app_wire;
use user::messenger::errno;
use user::messenger::services::init::wire::{
    self as init_wire, AppInfo, LaunchReply, RelaunchAppReply, ReloadState, ServiceStatus,
    ShutdownReply, StartDriverReply,
};
use user::sys::Cred as SysCred;
use user::trace;

use super::apps::{app_infos, GREETER_APP_ID};
use super::launch::launch;
use super::lifecycle;
use super::protocol::Supervisor;
use super::shutdown;
use super::state::{Service, CAP_SETUID, LAUNCH_CAP_PER_SESSION};
use super::stop::stop_app;

/// The credentials the kernel stamped on a request (issue #446).
pub(super) fn cred_of(cx: &Caller) -> SysCred {
    SysCred::new(cx.uid, cx.gid, cx.caps, cx.label_id, cx.session)
}

/// One request against the supervisor's tables.
pub(super) struct Handler<'a, 'b> {
    pub(super) state: &'a mut Supervisor<'b>,
    /// The request's transaction, when it expects a reply.
    pub(super) txn: Option<u64>,
    /// The reply is sent later (`Stop`, `Home`): nothing answers now.
    pub(super) held: bool,
}

impl Handler<'_, '_> {
    fn hold(&mut self) -> Fault {
        self.held = true;
        Fault::unserved()
    }
}

impl init_wire::Server for Handler<'_, '_> {
    fn services(&mut self, _: &Caller) -> Result<Vec<ServiceStatus>, Fault> {
        Ok(self.state.cache.rows(self.state.services))
    }

    fn launch(
        &mut self,
        cx: &Caller,
        app: String,
        args: String,
        session: u64,
    ) -> Result<LaunchReply, Fault> {
        // Nothing new starts once the machine is going down.
        if shutdown::stopping() {
            return Err(Fault::from_syscall(errno::EBUSY));
        }
        if app.is_empty() {
            return Err(Fault::from_syscall(errno::EINVAL));
        }
        let request = init_wire::LaunchArgs { app, args, session };
        let Supervisor {
            services,
            broker,
            installed,
            logouts,
            feeds,
            ..
        } = &mut *self.state;
        // `logind` publishes a record and then sends its request, but the
        // broker rings the feed's bell only after it answered the publisher:
        // a record not read yet is looked for on the broker itself. Only a
        // caller that could own the session (`CAP_SETUID`, `logind`) earns
        // that wait, so a bogus session id from anyone else cannot stall
        // `init` (`launch` refuses it).
        if cx.caps & CAP_SETUID != 0 {
            let ended = if request.app == GREETER_APP_ID {
                // The old session's `login/end` first, so the hold below sees
                // it; one tick at most.
                feeds.catch_up_end(services)
            } else if request.session != 0 && super::sessions::owner(request.session).is_none() {
                feeds.catch_up(services)
            } else {
                Vec::new()
            };
            for session in ended {
                logouts.begin(services, broker, session);
            }
        }
        // The login screen comes back once the ended session's apps are gone
        // (or their grace ran out): `logind` retries.
        if request.app == GREETER_APP_ID && logouts.holds_greeter() {
            trace::line!(trace::Channel::Init, "LOGOUT:GREETER:HELD");
            return Err(Fault::from_syscall(errno::EAGAIN));
        }
        let caller = installed.owner_cred(cred_of(cx));
        match launch(services, broker, installed, &request, &caller, false) {
            Ok(result) => Ok(result),
            Err(error) => {
                let target = if request.session == 0 {
                    caller.session
                } else {
                    request.session
                };
                if error.errno() == Some(-errno::EPERM) {
                    trace::line!(
                        trace::Channel::Init,
                        "LAUNCH:DENIED:PASS app={} caller_uid={} caller_session={} target={}",
                        request.app,
                        caller.uid,
                        caller.session,
                        target
                    );
                } else if error.errno() == Some(-errno::EAGAIN) {
                    trace::line!(
                        trace::Channel::Init,
                        "LAUNCH:CAP:PASS app={} session={} cap={}",
                        request.app,
                        target,
                        LAUNCH_CAP_PER_SESSION
                    );
                }
                Err(error.into())
            }
        }
    }

    /// Built-ins first, in registry order, then what the package manager
    /// installed, read afresh so the menu is never stale.
    fn list_apps(&mut self, cx: &Caller) -> Result<Vec<AppInfo>, Fault> {
        self.state.installed.refresh();
        Ok(self.state.installed.infos(app_infos(), cx.uid))
    }

    fn stop(&mut self, cx: &Caller, app: String) -> Result<u64, Fault> {
        if app.is_empty() {
            return Err(Fault::from_syscall(errno::EINVAL));
        }
        let state = &mut *self.state;
        let stopped = stop_app(state.services, state.broker, &app, &cred_of(cx))?;
        // Answered once every target has been reaped.
        let Some(txn) = self.txn else {
            return Ok(stopped.count);
        };
        self.state
            .stops
            .hold(txn, &app, stopped.pids, stopped.count);
        Err(self.hold())
    }

    fn shutdown(
        &mut self,
        cx: &Caller,
        mode: u32,
        reason: String,
        force: bool,
    ) -> Result<ShutdownReply, Fault> {
        let request = init_wire::ShutdownArgs {
            mode,
            reason,
            force,
        };
        let Supervisor {
            services,
            broker,
            installed,
            shutdown,
            ..
        } = &mut *self.state;
        // The Terminal, the user's shell, is the session's owner.
        let caller = installed.owner_cred(cred_of(cx));
        let phase = shutdown::request(shutdown, services, broker, &request, &caller)?;
        Ok(ShutdownReply {
            accepted: true,
            phase,
        })
    }

    /// `devd` asks for a driver row for the device it matched.
    fn start_driver(
        &mut self,
        cx: &Caller,
        driver: String,
        device: u64,
        ifname: String,
    ) -> Result<StartDriverReply, Fault> {
        let request = init_wire::StartDriverArgs {
            driver,
            device,
            ifname,
        };
        let state = &mut *self.state;
        Ok(super::drivers::start(
            state.services,
            state.broker,
            cx.sender,
            &request,
        )?)
    }

    /// A service says it serves: what waited for it may start.
    fn ready(&mut self, cx: &Caller) {
        let state = &mut *self.state;
        if super::ready::observe(state.services, cx.sender) && !shutdown::stopping() {
            super::supervise::start_ready(state.services, state.broker);
        }
    }

    /// A launched app says why it is failing (issue #549).
    fn report_failure(&mut self, cx: &Caller, reason: String) {
        super::notice::report(self.state.services, cx.sender, &reason);
    }

    /// Answers once its helper exits (`homes.rs`).
    fn home(
        &mut self,
        cx: &Caller,
        op: String,
        name: String,
        uid: u32,
        gid: u32,
    ) -> Result<(), Fault> {
        let request = init_wire::HomeArgs { op, name, uid, gid };
        self.state.homes.start(cx, self.txn, request)?;
        Err(self.hold())
    }

    /// `elevd` restarts a service an administrator approved (U2).
    fn restart_service(&mut self, cx: &Caller, name: String) -> Result<u64, Fault> {
        super::homes::restart_service(self.state.services, cx, &name)
    }

    /// `dbgd` hot-reloads a service (docs/dbgd-plan.md, v2).
    fn reload_service(
        &mut self,
        cx: &Caller,
        name: String,
        binary: String,
        sha256: String,
        trial_ms: u32,
    ) -> Result<u64, Fault> {
        let request = init_wire::ReloadServiceArgs {
            name,
            binary,
            sha256,
            trial_ms,
        };
        Ok(super::reload::reload(self.state.services, cx, &request)?)
    }

    fn revert_service(&mut self, cx: &Caller, name: String) -> Result<u64, Fault> {
        Ok(super::reload::revert(self.state.services, cx, &name)?)
    }

    fn relaunch_app(&mut self, cx: &Caller, app: String) -> Result<RelaunchAppReply, Fault> {
        let state = &mut *self.state;
        Ok(super::relaunch::relaunch(
            state.services,
            state.broker,
            state.installed,
            cx,
            &app,
        )?)
    }

    fn reloads(&mut self, _: &Caller) -> Result<Vec<ReloadState>, Fault> {
        Ok(super::reload::list(self.state.services))
    }
}

/// The app lifecycle line: `Watch` on its own endpoint.
pub(super) struct AppHandler<'a> {
    pub(super) services: &'a mut [Service],
}

impl app_wire::Server for AppHandler<'_> {
    fn watch(&mut self, cx: &Caller, events: u64) -> Result<(), Fault> {
        Ok(lifecycle::watch(self.services, cx.sender, events)?)
    }
}
