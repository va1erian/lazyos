//! `netd`'s configuration from `confd` (`sys/net/<if>/{mode,address,gateway,dns}`,
//! docs/networking-plan.md section 6), read-only for `netd`, one set per
//! interface.
//!
//! `confd` is a **soft** dependency: tried for about a second at start, DHCP
//! when it is absent, looked for again from the main loop. Values go through
//! `netstack::config`, which accepts a static setup only when every part of it
//! parses and makes sense, and otherwise falls back to DHCP.

use alloc::format;
use alloc::string::String;

use confd::Value;
use netstack::config::{Mode, Raw};
use user::messenger::confd::Client;

const START_ATTEMPTS: usize = 20;
/// Ticks between re-reads while running.
pub(super) const REFRESH_TICKS: u64 = 500;

pub(super) struct Config {
    client: Option<Client>,
}

impl Config {
    /// Wait for `confd` for about a second.
    pub(super) fn load() -> Config {
        Config {
            client: Client::connect_retry(START_ATTEMPTS).ok(),
        }
    }

    /// Read `interface`'s configuration; `None` when `confd` is unreachable
    /// (the caller keeps what it has). A missing client is looked for again,
    /// so a `confd` that started late, or restarted, is picked up.
    pub(super) fn read(&mut self, interface: &str) -> Option<Mode> {
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
        Some(read(client, interface))
    }
}

fn text(client: &Client, interface: &str, key: &str) -> Option<String> {
    match client
        .get(&format!("sys/net/{interface}/{key}"))
        .ok()
        .flatten()?
    {
        Value::Str(text) => Some(text),
        _ => None,
    }
}

fn read(client: &Client, interface: &str) -> Mode {
    let (mode, address, gateway, dns) = (
        text(client, interface, "mode"),
        text(client, interface, "address"),
        text(client, interface, "gateway"),
        text(client, interface, "dns"),
    );
    Mode::from_raw(&Raw {
        mode: mode.as_deref(),
        address: address.as_deref(),
        gateway: gateway.as_deref(),
        dns: dns.as_deref(),
    })
}
