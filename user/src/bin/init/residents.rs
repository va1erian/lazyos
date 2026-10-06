//! The retained `session/<s>/apps/resident` topic (docs/tray-plan.md
//! section 5): each session's running resident apps, which put a default
//! item on that session's tray for every one of them.
//!
//! `init` republishes a session's list whenever a resident row of it starts
//! or ends. A cheap fingerprint over those rows says when anything changed,
//! so an idle supervisor neither allocates nor publishes (its allocator never
//! reclaims). The broker is `messengerd` on the central bus, as for the
//! app-failure notices; a wedged broker costs an update, never the loop.
//! Serial: `INIT:APP:RESIDENT session=<s> apps=<n>`.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use user::central;
use user::messenger::{registry, services, topics_client};
use user::sys;

use super::state::{Phase, Service};

/// Ticks one publish may take (100 Hz).
const PUBLISH_TICKS: u64 = 50;

/// What was last published, and where to.
pub(super) struct Residents {
    bus: Option<central::Bus>,
    fingerprint: u64,
    /// Sessions that were last told of at least one resident app (so they
    /// hear when the last one goes).
    sessions: Vec<u64>,
}

/// A resident row that holds a task.
fn running(row: &Service) -> bool {
    row.launched && row.life.resident && matches!(row.phase, Phase::Running | Phase::Stopping)
}

fn session_of(row: &Service) -> u64 {
    row.cred.map_or(0, |cred| cred.session)
}

impl Residents {
    pub(super) const fn new() -> Residents {
        Residents {
            bus: None,
            // Nothing ran yet, so there is nothing to tell.
            fingerprint: EMPTY,
            sessions: Vec::new(),
        }
    }

    /// Publish every session whose resident apps changed since last time.
    pub(super) fn sync(&mut self, services: &[Service]) {
        let fingerprint = fingerprint(services);
        if fingerprint == self.fingerprint {
            return;
        }
        let mut now: Vec<u64> = services
            .iter()
            .filter(|row| running(row))
            .map(session_of)
            .collect();
        now.sort_unstable();
        now.dedup();
        let mut all = now.clone();
        all.extend(
            self.sessions
                .iter()
                .copied()
                .filter(|session| !now.contains(session)),
        );
        let mut ok = true;
        for session in all {
            let apps: Vec<services::init::wire::ResidentApp> = services
                .iter()
                .filter(|row| running(row) && session_of(row) == session)
                .map(|row| services::init::wire::ResidentApp {
                    app: row.name.to_string(),
                    pid: row.pid,
                })
                .collect();
            ok &= self.publish(session, apps);
        }
        // A failed publish is tried again on the next pass.
        if ok {
            self.fingerprint = fingerprint;
            self.sessions = now;
        }
    }

    fn publish(&mut self, session: u64, apps: Vec<services::init::wire::ResidentApp>) -> bool {
        let count = apps.len();
        let value = services::init::wire::ResidentApps { apps };
        let mut outcome = self.try_publish(session, &value);
        if outcome.is_err() {
            self.bus = None;
            outcome = self.try_publish(session, &value);
        }
        match outcome {
            Ok(_) => {
                sys::write_str(&format!(
                    "INIT:APP:RESIDENT session={session} apps={count}\n"
                ));
                true
            }
            Err(code) => {
                sys::write_str(&format!(
                    "INIT:APP:RESIDENT:FAIL session={session} err={}\n",
                    -code
                ));
                false
            }
        }
    }

    fn try_publish(
        &mut self,
        session: u64,
        value: &services::init::wire::ResidentApps,
    ) -> Result<u64, i64> {
        use services::init::wire;
        let session: String = session.to_string();
        let topic = wire::name_session_apps_resident(&session).map_err(|_| -22i64)?;
        let payload = wire::encode_session_apps_resident(value).map_err(|_| -22i64)?;
        if self.bus.is_none() {
            let endpoint = registry::resolve(topics_client::NAME)
                .map_err(|error| error.errno().unwrap_or(-5))?;
            self.bus = Some(central::Bus::from_endpoint(endpoint));
        }
        let bus = self.bus.as_mut().ok_or(-1)?;
        bus.publish_by(
            &topic,
            &payload,
            wire::TOPIC_SESSION_APPS_RESIDENT_RETAINED,
            Some(sys::clock() + PUBLISH_TICKS),
        )
        .map_err(|error| error.errno().unwrap_or(-5))
    }
}

/// The FNV-1a basis: the fingerprint of no resident app at all.
const EMPTY: u64 = 0xcbf2_9ce4_8422_2325;

/// FNV-1a over every running resident row's session, name and pid.
fn fingerprint(services: &[Service]) -> u64 {
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = EMPTY;
    for row in services.iter().filter(|row| running(row)) {
        let bytes = session_of(row)
            .to_le_bytes()
            .into_iter()
            .chain(row.name.bytes())
            .chain(row.pid.to_le_bytes());
        for byte in bytes {
            hash = (hash ^ u64::from(byte)).wrapping_mul(PRIME);
        }
    }
    hash
}
