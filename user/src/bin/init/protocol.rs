//! `init`'s request protocol: the cached `Services` reply, the supervision
//! table's wire rows, and the non-blocking dispatch of broker/registry/launch
//! messages on the supervisor endpoint.
//!
//! Split out of `init.rs` (issue #194); `Shutdown` and the `Launch` refusal
//! while it runs are docs/shutdown.md.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use user::messenger::{self, router, services, Endpoint, Message, Parcel};
use user::sys;

use super::apps::app_infos;
use super::installed::InstalledApps;
use super::launch::{actor, launch};
use super::sessions;
use super::shutdown::{self, Shutdown};
use super::state::{Service, LAUNCH_CAP_PER_SESSION};
use super::stop::stop_app;

/// Cached `Services` reply.
///
/// The user runtime's bump allocator never reclaims memory, and every encoded
/// reply allocates several small buffers, so the supervisor re-encodes its
/// supervision table only when a cheap fingerprint (phase/pid/restarts) says
/// it changed; callers get a clone of the encoded parcel.
#[derive(Default)]
pub(super) struct StatusCache {
    fingerprint: Option<u64>,
    parcel: Option<Parcel>,
}

impl StatusCache {
    /// The current reply, re-encoding the table when it changed.
    fn parcel(&mut self, services: &[Service]) -> messenger::Result<Parcel> {
        const PRIME: u64 = 0x0000_0100_0000_01b3;
        let mut fingerprint = 0xcbf2_9ce4_8422_2325u64;
        for service in services {
            for byte in service
                .phase
                .label()
                .bytes()
                .chain(service.pid.to_le_bytes())
                .chain(service.restarts.to_le_bytes())
            {
                fingerprint ^= byte as u64;
                fingerprint = fingerprint.wrapping_mul(PRIME);
            }
        }
        if self.fingerprint != Some(fingerprint) || self.parcel.is_none() {
            self.parcel = Some(services::services_reply(&status_rows(services))?);
            self.fingerprint = Some(fingerprint);
        }
        Ok(self.parcel.clone().unwrap_or_default())
    }
}

/// What the supervisor's request handlers share: its tables and the
/// shutdown, if one is running.
pub(super) struct Supervisor<'a> {
    pub(super) services: &'a mut Vec<Service>,
    pub(super) broker: &'a mut router::TopicBroker,
    pub(super) installed: &'a mut InstalledApps,
    pub(super) cache: &'a mut StatusCache,
    pub(super) shutdown: &'a mut Option<Shutdown>,
}

/// Serve queued subscriptions and control calls without blocking.
pub(super) fn serve_pending(
    state: &mut Supervisor,
    server: &Endpoint,
    buffer: &mut [u8],
) -> messenger::Result<()> {
    while let Some(message) = server.poll_recv_with(buffer)? {
        let interface = message.interface_id();
        let method = message.method();
        let reply = match dispatch(state, &message) {
            Ok(parcel) => parcel,
            // A malformed request still gets an answer, or its caller would
            // wait forever. A structured error is the useful one on the
            // control interface; the topic router keeps an empty reply.
            Err(error) if interface == services::INIT_INTERFACE => {
                services::init_error_reply(method, error)
            }
            Err(_) => Parcel::default(),
        };
        if let Some(txn) = message.txn {
            server.reply_or_drop(txn, &reply)?;
        }
    }
    Ok(())
}

/// Dispatch one inbound message to the broker, the supervision table, the
/// app registry / launch path, or the shutdown.
fn dispatch(state: &mut Supervisor, message: &Message) -> messenger::Result<Parcel> {
    let Supervisor {
        services,
        broker,
        installed,
        cache,
        shutdown,
    } = state;
    match message.interface_id() {
        router::INTERFACE => {
            // `logind`'s login events also tell the launch path who owns
            // each session (see `sessions.rs`).
            sessions::observe(services, message);
            // `pkgd`'s provisioning progress, for the autostart.
            super::provisioning::observe(services, message);
            broker.handle(message)
        }
        services::INIT_INTERFACE => match message.method() {
            services::init::METHOD_SERVICES => cache.parcel(services),
            services::init::METHOD_LISTAPPS => {
                // Built-ins first, in registry order, then what the package
                // manager installed, read afresh so the menu is never stale.
                installed.refresh();
                let caller = actor(message)?;
                let apps = installed.infos(app_infos(), caller.uid);
                services::list_apps_reply(&apps)
            }
            services::init::METHOD_STOP => {
                let request = services::decode_stop_request(&message.parcel)?;
                let caller = actor(message)?;
                let stopped = stop_app(services, broker, &request.app, &caller)?;
                services::stop_reply(stopped)
            }
            // A service says it serves: what waited for it may start.
            services::init::METHOD_READY => {
                if super::ready::observe(services, message) && !shutdown::stopping() {
                    super::supervise::start_ready(services, broker);
                }
                Ok(Parcel::default())
            }
            // A launched app says why it is failing (issue #549).
            services::init::wire::METHOD_REPORTFAILURE => {
                super::notice::report(services, message);
                Ok(Parcel::default())
            }
            services::init::METHOD_SHUTDOWN => {
                let request = services::init::wire::decode_shutdown_args(&message.parcel.body)
                    .map_err(messenger::Error::Parcel)?;
                let caller = actor(message)?;
                let phase = shutdown::request(shutdown, services, broker, &request, &caller)?;
                services::shutdown_reply(true, &phase)
            }
            // Nothing new starts once the machine is going down.
            services::init::METHOD_LAUNCH if shutdown::stopping() => {
                Err(messenger::Error::Errno(-messenger::errno::EBUSY))
            }
            services::init::METHOD_LAUNCH => {
                let request = services::decode_launch_request(&message.parcel)?;
                let caller = actor(message)?;
                match launch(services, broker, installed, &request, &caller, false) {
                    Ok(result) => services::launch_reply(&result),
                    Err(error) => {
                        let target = if request.session == 0 {
                            caller.session
                        } else {
                            request.session
                        };
                        if error.errno() == Some(-messenger::errno::EPERM) {
                            sys::write_str(&format!(
                                "INIT:LAUNCH:DENIED:PASS app={} caller_uid={} caller_session={} target={}\n",
                                request.app, caller.uid, caller.session, target
                            ));
                        } else if error.errno() == Some(-messenger::errno::EAGAIN) {
                            sys::write_str(&format!(
                                "INIT:LAUNCH:CAP:PASS app={} session={} cap={}\n",
                                request.app, target, LAUNCH_CAP_PER_SESSION
                            ));
                        }
                        Err(error)
                    }
                }
            }
            _ => Err(messenger::Error::Errno(-messenger::errno::EINVAL)),
        },
        _ => Err(messenger::Error::Errno(-messenger::errno::EINVAL)),
    }
}

/// Snapshot the supervision table as wire rows.
fn status_rows(services: &[Service]) -> Vec<services::ServiceStatus> {
    services
        .iter()
        .map(|service| {
            let deps = service.deps.iter().fold(String::new(), |mut text, dep| {
                if !text.is_empty() {
                    text.push(',');
                }
                text.push_str(dep);
                text
            });
            services::ServiceStatus {
                name: service.name.to_string(),
                state: service.phase.label().to_string(),
                pid: service.pid,
                restarts: service.restarts,
                deps,
                health: service.phase.health().to_string(),
            }
        })
        .collect()
}
