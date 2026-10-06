//! The driver's tunables from `confd` (`docs/driver-config-plan.md` section 2,
//! keys `sys/dev/net/virtio-net/*`, or `sys/dev/net/e1000/*` for an 8254x).
//!
//! `confd` is a **soft** dependency: the driver tries to resolve it for a
//! second at most, runs on the defaults when it is absent, and looks again
//! from its main loop. It `Get`s only its own known keys, and every value goes
//! through `virtio_net::settings`, which clamps or defaults anything absent,
//! mistyped, out of range or hostile. Nothing is allocated from an unclamped
//! value and nothing here can panic on bad input.

use alloc::format;
use alloc::string::String;

use confd::Value;
use user::messenger::confd::Client;
use virtio_net::settings::{Raw, Settings};

use super::device::Kind;

/// Resolve attempts at start (about a second of ticks).
const START_ATTEMPTS: usize = 20;
/// Ticks between re-reads of the keys while the driver runs.
pub(super) const REFRESH_TICKS: u64 = 500;

/// The key prefix for a card.
pub(super) fn prefix(kind: Kind) -> &'static str {
    match kind {
        Kind::Virtio => "sys/dev/net/virtio-net",
        Kind::E1000(_) => "sys/dev/net/e1000",
    }
}

pub(super) struct Config {
    client: Option<Client>,
    prefix: &'static str,
}

impl Config {
    /// Connect to `confd` if it is up and read the settings under `prefix`.
    pub(super) fn load(prefix: &'static str) -> (Config, Settings) {
        let client = Client::connect_retry(START_ATTEMPTS).ok();
        let settings = client
            .as_ref()
            .map_or(Settings::DEFAULT, |client| read(client, prefix));
        (Config { client, prefix }, settings)
    }

    /// Read the settings again; `None` when `confd` is not reachable (the
    /// caller keeps what it has). A missing client is looked for again, so a
    /// `confd` that started late, or restarted, is picked up.
    pub(super) fn refresh(&mut self) -> Option<Settings> {
        if self.client.is_none() {
            self.client = Client::connect().ok();
        }
        let client = self.client.as_ref()?;
        // A cheap probe: if the registry answers nothing at all, treat it as
        // gone and reconnect next time.
        if client.info().is_err() {
            self.client = None;
            return None;
        }
        Some(read(client, self.prefix))
    }
}

fn get(client: &Client, prefix: &str, key: &str) -> Option<Value> {
    client.get(&format!("{prefix}/{key}")).ok().flatten()
}

fn number(client: &Client, prefix: &str, key: &str) -> Option<u64> {
    match get(client, prefix, key)? {
        Value::U64(n) => Some(n),
        Value::I64(n) if n >= 0 => Some(n as u64),
        _ => None,
    }
}

fn text(client: &Client, prefix: &str, key: &str) -> Option<String> {
    match get(client, prefix, key)? {
        Value::Str(t) => Some(t),
        _ => None,
    }
}

fn read(client: &Client, prefix: &str) -> Settings {
    let irq_mode = text(client, prefix, "irq_mode");
    let mac = text(client, prefix, "mac_override");
    Settings::from_raw(&Raw {
        irq_mode: irq_mode.as_deref(),
        poll_interval_ms: number(client, prefix, "poll_interval_ms"),
        rx_ring_entries: number(client, prefix, "rx_ring_entries"),
        tx_ring_entries: number(client, prefix, "tx_ring_entries"),
        mtu: number(client, prefix, "mtu"),
        mac_override: mac.as_deref(),
    })
}
