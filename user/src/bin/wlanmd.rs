//! `wlanmd` (`/system/bin/wlanmd`): the Wi-Fi station manager
//! (docs/wifi-prerequisites-plan.md WP4, docs/architecture/wifi.md).
//!
//! Runs as `_wlan` (uid 912, `netpolicy::WLAN_UID`) with **no capabilities**:
//! it parses the air (beacons, authentication, association, EAPOL) and holds
//! no device, no DMA and no authority over anyone. It
//!
//! * finds radios by listing the registry for `os.lazy.net.wifi.hw/<ifname>`
//!   (names only the Wi-Fi drivers `_wifi`/`_wifisim` may hold), attaches to
//!   each with an event channel and tells it the regulatory domain
//!   (`sys/net/wifi/country`, default `00`);
//! * runs one `wlansta::Station` per radio: scan, selection, authentication,
//!   association and the WPA2 handshake, with the PMK from `keyd` `WifiPmk`;
//! * serves `os.lazy.net.wifi.v1` as `os.lazy.net.wifi` (`service.rs`), keeps
//!   profiles in `confd` and passphrases in `keyd` (`store.rs`);
//! * publishes `system/net/<if>/wifi/state` (retained) and `.../wifi/scan`.
//!
//! A radio that vanishes (its driver died) is dropped and its state published
//! `Disabled`; when the driver comes back it is attached again. With no
//! radio at all `wlanmd` idles and serves an empty `Interfaces`.
//!
//! Serial lines: `WLANMD:UP radios=<n>`, `WLANMD:RADIO:UP <if> mac=<mac>`,
//! `WLANMD:RADIO:GONE <if>`, `WLANMD:STATE <if> <state> reason=<reason>
//! ssid="<ssid>"`, `WLANMD:SCAN <if> gen=<n> networks=<n>`. No secret, PMK or
//! key is ever printed.

#![no_std]
#![no_main]

extern crate alloc;

#[path = "wlanmd/radio.rs"]
mod radio;
#[path = "wlanmd/service.rs"]
mod service;
#[path = "wlanmd/store.rs"]
mod store;

use alloc::format;
use alloc::vec::Vec;
use core::panic::PanicInfo;

use user::messenger::wifi::{self, api};
use user::messenger::{self, errno, registry, wait, Endpoint, Error};
use user::sys;

use radio::Slot;
use store::Store;

/// Ticks (100 Hz) between looks at the registry for radios.
const DISCOVER_TICKS: u64 = 100;
/// A radio missing from the registry this long is gone.
const GONE_TICKS: u64 = 300;
/// Radios served at once: `wait_any` watches the service endpoint and one
/// event endpoint per radio.
const MAX_RADIOS: usize = 4;

// `keyd` answers PMKs to `secretstore::WLAN_UID`, the registry and `confd`
// recognise `netpolicy::WLAN_UID`: one identity.
const _: () = assert!(secretstore::WLAN_UID == netpolicy::WLAN_UID);

/// The station's clock: milliseconds since boot.
pub(crate) fn now_ms() -> u64 {
    sys::clock().saturating_mul(lazyos_sys::time::TICK_MS)
}

/// A fresh 32-octet nonce from the kernel CSPRNG (the SNonce source).
fn nonce() -> [u8; 32] {
    let mut out = [0u8; 32];
    // Checked once at start (`run`); the pool does not run dry after.
    let _ = sys::random(&mut out);
    out
}

pub(crate) struct Daemon {
    pub radios: Vec<Slot>,
    pub store: Store,
    pub bus: Option<user::central::Bus>,
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    if let Err(error) = run() {
        sys::write_str(&format!("WLANMD:FAIL {}\n", error.message()));
        sys::exit(1);
    }
    sys::exit(0)
}

fn run() -> messenger::Result<()> {
    let mut probe = [0u8; 32];
    sys::random(&mut probe).map_err(Error::Errno)?;
    let (published, server) = messenger::create_pair()?;
    registry::register(wifi::NAME, &published, &[api::INTERFACE_ID], 0)?;
    let mut daemon = Daemon {
        radios: Vec::new(),
        store: Store::new(),
        bus: None,
    };
    daemon.discover();
    sys::write_str(&format!("WLANMD:UP radios={}\n", daemon.radios.len()));
    let mut buffer = alloc::vec![0u8; messenger::DEFAULT_BUFFER];
    let mut next_discovery = sys::clock() + DISCOVER_TICKS;
    loop {
        if sys::clock() >= next_discovery {
            daemon.discover();
            next_discovery = sys::clock() + DISCOVER_TICKS;
        }
        let deadline = daemon.next_deadline().map_or(next_discovery, |at| at.min(next_discovery));
        let mut endpoints: Vec<Endpoint> = Vec::with_capacity(1 + MAX_RADIOS);
        endpoints.push(server);
        endpoints.extend(daemon.radios.iter().map(|slot| slot.events));
        let ready = match wait::wait_any(&endpoints, 0, Some(deadline)) {
            Ok(ready) => ready,
            Err(Error::Errno(code)) if code == -errno::ETIMEDOUT => 0,
            Err(error) => return Err(error),
        };
        if ready & 1 != 0 {
            serve_pending(&mut daemon, server, &mut buffer);
        }
        daemon.radio_events(ready >> 1);
        daemon.tick();
        daemon.publish();
    }
}

/// Answer every request waiting on the service endpoint.
fn serve_pending(daemon: &mut Daemon, server: Endpoint, buffer: &mut [u8]) {
    while let Ok(Some(message)) = server.poll_recv_with(buffer) {
        let reply = service::dispatch(daemon, &message).unwrap_or_else(|error| {
            messenger::services::error_reply(message.interface_id(), message.method(), error)
        });
        if let Some(txn) = message.txn {
            // A caller whose deadline passed is a normal race.
            let _ = server.reply(txn, &reply);
        }
        daemon.publish();
    }
}

impl Daemon {
    /// The earliest station deadline, in ticks.
    fn next_deadline(&self) -> Option<u64> {
        self.radios
            .iter()
            .filter_map(|slot| slot.station.next_deadline())
            .min()
            .map(|ms| ms.div_ceil(lazyos_sys::time::TICK_MS))
    }

    /// Attach radios that appeared, drop the ones gone (and read the
    /// profiles once `confd` answers).
    fn discover(&mut self) {
        if !self.store.loaded {
            self.reload_profiles();
        }
        let Ok(names) = wifi::radios() else {
            return;
        };
        let now = sys::clock();
        for slot in self.radios.iter_mut() {
            if names.contains(&slot.ifname) {
                slot.seen = now;
            }
        }
        let gone: Vec<usize> = (0..self.radios.len())
            .filter(|&i| now.saturating_sub(self.radios[i].seen) > GONE_TICKS)
            .collect();
        for index in gone.into_iter().rev() {
            self.drop_radio(index);
        }
        for ifname in names {
            if self.radios.len() >= MAX_RADIOS || self.radios.iter().any(|s| s.ifname == ifname) {
                continue;
            }
            let country = self.store.country();
            match Slot::attach(&ifname, &country, nonce) {
                Ok(slot) => {
                    sys::write_str(&format!(
                        "WLANMD:RADIO:UP {ifname} mac={} country={country}\n",
                        radio::mac_text(&slot.mac)
                    ));
                    self.radios.push(slot);
                    let index = self.radios.len() - 1;
                    let profiles = self.store.profiles().to_vec();
                    let actions = self.radios[index].station.set_profiles(now_ms(), profiles);
                    self.radios[index].execute(&mut self.store, actions);
                    let actions = self.radios[index].station.start(now_ms());
                    self.radios[index].execute(&mut self.store, actions);
                }
                Err(error) => sys::write_str(&format!(
                    "WLANMD:RADIO:FAIL {ifname} {}\n",
                    error.message()
                )),
            }
        }
    }

    /// Forget a radio whose driver went away: publish it `Disabled`.
    pub fn drop_radio(&mut self, index: usize) {
        let slot = self.radios.remove(index);
        sys::write_str(&format!("WLANMD:RADIO:GONE {}\n", slot.ifname));
        if let Some(bus) = self.bus.as_mut() {
            let state = api::WifiState {
                state: api::CONN_STATE_DISABLED,
                changes: slot.station.status().changes.wrapping_add(1),
                ..api::WifiState::default()
            };
            let _ = api::publish_system_net_wifi_state(bus, &slot.ifname, &state);
        }
        slot.close();
    }

    /// Read the events of the radios whose bit is set in `ready`.
    fn radio_events(&mut self, ready: u64) {
        let mut index = 0;
        while index < self.radios.len() {
            let alive = if ready & (1 << index) != 0 {
                self.radios[index].drain_events(&mut self.store)
            } else {
                true
            };
            if alive {
                index += 1;
            } else {
                self.drop_radio(index);
            }
        }
    }

    /// Run the timers that are due.
    fn tick(&mut self) {
        let now = now_ms();
        for slot in self.radios.iter_mut() {
            if slot.station.next_deadline().is_some_and(|at| at <= now) {
                let actions = slot.station.input(now, wlansta::Event::Tick);
                slot.execute(&mut self.store, actions);
            }
        }
    }

    /// Publish each radio's state and scan generation when they changed.
    pub fn publish(&mut self) {
        if self.bus.is_none() {
            self.bus = user::central::Bus::connect().ok();
        }
        for slot in self.radios.iter_mut() {
            slot.publish(self.bus.as_mut());
        }
    }

    /// Hand every station the current profiles.
    pub fn reload_profiles(&mut self) {
        self.store.reload();
        let profiles = self.store.profiles().to_vec();
        for slot in self.radios.iter_mut() {
            let actions = slot.station.set_profiles(now_ms(), profiles.clone());
            slot.execute(&mut self.store, actions);
        }
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    sys::write_str("WLANMD:FAIL panic\n");
    sys::exit(1)
}
