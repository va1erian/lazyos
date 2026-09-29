//! `init`'s request protocol: the cached `Services` reply, the supervision
//! table's wire rows, and the non-blocking dispatch of broker/registry/launch
//! messages on the supervisor endpoint.
//!
//! Split out of `init.rs` (issue #194); a pure move, no behavior change.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use user::messenger::{self, router, services, Endpoint, Message, Parcel};
use user::sys;

use super::apps::app_infos;
use super::launch::{actor, launch};
use super::state::{Service, LAUNCH_CAP_PER_SESSION};

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

/// Serve queued subscriptions and control calls without blocking.
pub(super) fn serve_pending(
    services: &mut Vec<Service>,
    broker: &mut router::TopicBroker,
    server: &Endpoint,
    buffer: &mut [u8],
    cache: &mut StatusCache,
) -> messenger::Result<()> {
    while let Some(message) = server.poll_recv_with(buffer)? {
        let interface = message.interface_id();
        let method = message.method();
        let reply = match dispatch(services, broker, &message, cache) {
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

/// Dispatch one inbound message to the broker, the supervision table, or the
/// app registry / launch path.
fn dispatch(
    services: &mut Vec<Service>,
    broker: &mut router::TopicBroker,
    message: &Message,
    cache: &mut StatusCache,
) -> messenger::Result<Parcel> {
    match message.interface_id() {
        router::INTERFACE => broker.handle(message),
        services::INIT_INTERFACE => match message.method() {
            services::init_method::SERVICES => cache.parcel(services),
            services::init_method::LIST_APPS => services::list_apps_reply(&app_infos()),
            services::init_method::LAUNCH => {
                let request = services::decode_launch_request(&message.parcel)?;
                let caller = actor(message)?;
                match launch(services, broker, &request, &caller) {
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
