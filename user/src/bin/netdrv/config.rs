//! The driver's tunables from `confd` (`docs/driver-config-plan.md` section 2,
//! keys `sys/dev/net/virtio-net/*`).
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

/// Resolve attempts at start (about a second of ticks).
const START_ATTEMPTS: usize = 20;
/// Ticks between re-reads of the keys while the driver runs.
pub(super) const REFRESH_TICKS: u64 = 500;

const PREFIX: &str = "sys/dev/net/virtio-net";

pub(super) struct Config {
    client: Option<Client>,
}

impl Config {
    /// Connect to `confd` if it is up and read the settings.
    pub(super) fn load() -> (Config, Settings) {
        let client = Client::connect_retry(START_ATTEMPTS).ok();
        let settings = client.as_ref().map_or(Settings::DEFAULT, read);
        (Config { client }, settings)
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
        Some(read(client))
    }
}

fn get(client: &Client, key: &str) -> Option<Value> {
    client.get(&format!("{PREFIX}/{key}")).ok().flatten()
}

fn number(client: &Client, key: &str) -> Option<u64> {
    match get(client, key)? {
        Value::U64(n) => Some(n),
        Value::I64(n) if n >= 0 => Some(n as u64),
        _ => None,
    }
}

fn text(client: &Client, key: &str) -> Option<String> {
    match get(client, key)? {
        Value::Str(t) => Some(t),
        _ => None,
    }
}

fn read(client: &Client) -> Settings {
    let irq_mode = text(client, "irq_mode");
    let mac = text(client, "mac_override");
    Settings::from_raw(&Raw {
        irq_mode: irq_mode.as_deref(),
        poll_interval_ms: number(client, "poll_interval_ms"),
        rx_ring_entries: number(client, "rx_ring_entries"),
        tx_ring_entries: number(client, "tx_ring_entries"),
        mtu: number(client, "mtu"),
        mac_override: mac.as_deref(),
    })
}
