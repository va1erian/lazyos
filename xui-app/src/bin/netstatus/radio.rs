//! The applet's Wi-Fi half: follows one wireless interface through the
//! retained state topic and a few bounded calls on `wlanmd`, keeps the list
//! of heard networks, and does what a menu row asks.
//!
//! Nothing here parks the applet: the state arrives on the topic, and the
//! calls (`Status` for the signal, `Networks`, `Scan`) are made on a timer,
//! each bounded by `wifi::CALL_TICKS`; a call that fails pushes the next
//! one back, so a dead `wlanmd` costs one short wait every few seconds, not
//! a stall. What decides the icon, the tooltip and the menu is the pure
//! crate `wifitray`.

use messenger_generated::os_lazy_net_wifi_v1 as wire;
use wifitray::{Action, Link, Menu, NetworkRow, State, NETWORK_WIFI_ARG};
use xui_app::net::wifi::{self, ConnState, Security};

/// Ticks (100 Hz) between looks at the signal while connected.
const STATUS_TICKS: u64 = 200;
/// Ticks between reads of the heard networks.
const NETWORKS_TICKS: u64 = 1000;
/// Ticks between scans while not connected.
const SCAN_TICKS: u64 = 3000;
/// Ticks to wait after a failed call.
const BACKOFF_TICKS: u64 = 1000;

/// One wireless interface and what is known of it.
pub struct Radio {
    pub ifname: String,
    pub link: Link,
    heard: Vec<NetworkRow>,
    pub menu: Menu,
    next_status: u64,
    next_networks: u64,
    next_scan: u64,
}

/// How the applet groups `wlanmd`'s connection states.
fn state_of(state: ConnState) -> State {
    match state {
        ConnState::Disabled => State::Unavailable,
        ConnState::Idle => State::Idle,
        ConnState::Scanning => State::Searching,
        ConnState::Authenticating | ConnState::Associating | ConnState::Handshake => {
            State::Joining
        }
        ConnState::Connected => State::Connected,
        ConnState::Failed => State::Failed,
    }
}

impl Radio {
    pub fn new(ifname: &str) -> Radio {
        let link = Link::unavailable();
        Radio {
            ifname: ifname.to_owned(),
            menu: wifitray::menu::build(&link, &[]),
            link,
            heard: Vec::new(),
            next_status: 0,
            next_networks: 0,
            next_scan: 0,
        }
    }

    /// Take a state: from the retained topic or from a `Status` reply.
    /// Returns whether the link changed.
    fn set_state(
        &mut self,
        state: ConnState,
        ssid: &str,
        rssi_dbm: i32,
        freq_mhz: u32,
        reason: u32,
    ) -> bool {
        let state = state_of(state);
        let link = Link {
            state,
            ssid: ssid.to_owned(),
            bars: wifi::bars(rssi_dbm),
            rssi_dbm,
            band: wifi::band_label(freq_mhz),
            failure: (state == State::Failed)
                .then(|| wifi::reason_text(reason))
                .flatten()
                .map(str::to_owned),
            address: self.link.address,
        };
        if link == self.link {
            return false;
        }
        let joined_or_left = link.state != self.link.state || link.ssid != self.link.ssid;
        self.link = link;
        if joined_or_left {
            // The network list marks the one in use: read it again now.
            self.next_networks = 0;
        }
        self.rebuild_menu();
        true
    }

    /// The retained `system/net/<if>/wifi/state` value.
    pub fn apply_topic(&mut self, state: &wire::WifiState) -> bool {
        self.set_state(
            ConnState::from_wire(state.state),
            &state.ssid,
            state.rssi_dbm,
            state.freq_mhz,
            state.reason,
        )
    }

    /// A scan finished: the list changed.
    pub fn scan_done(&mut self) {
        self.next_networks = 0;
    }

    /// The address `netd` leased on the interface.
    pub fn set_address(&mut self, address: Option<[u8; 4]>) -> bool {
        if self.link.address == address {
            return false;
        }
        self.link.address = address;
        true
    }

    fn rebuild_menu(&mut self) {
        self.menu = wifitray::menu::build(&self.link, &self.heard);
    }

    /// Make the calls that are due at `now` (PIT ticks). Returns whether
    /// anything shown changed.
    pub fn tick(&mut self, now: u64) -> bool {
        let mut changed = false;
        if self.link.state == State::Connected && now >= self.next_status {
            self.next_status = now + STATUS_TICKS;
            match wifi::status(&self.ifname) {
                Ok(status) => {
                    changed |= self.set_state(
                        status.state,
                        &status.ssid,
                        status.rssi_dbm,
                        status.freq_mhz,
                        status.reason,
                    );
                }
                Err(_) => self.next_status = now + BACKOFF_TICKS,
            }
        }
        if self.link.state != State::Unavailable && now >= self.next_networks {
            self.next_networks = now + NETWORKS_TICKS;
            match wifi::networks(&self.ifname) {
                Ok(list) => changed |= self.set_heard(list),
                Err(_) => self.next_networks = now + BACKOFF_TICKS,
            }
        }
        if matches!(self.link.state, State::Idle | State::Failed) && now >= self.next_scan {
            self.next_scan = now + SCAN_TICKS;
            // `Busy` or `Again` are fine: a scan is running or came too soon.
            let _ = wifi::scan(&self.ifname);
        }
        changed
    }

    fn set_heard(&mut self, list: Vec<wifi::Network>) -> bool {
        let rows: Vec<NetworkRow> = list
            .iter()
            .map(|network| NetworkRow {
                ssid: network.ssid.clone(),
                secured: network.security.locked(),
                supported: network.security != Security::Unsupported,
                bars: network.bars(),
                rssi_dbm: network.rssi_dbm,
                saved_id: network.id.clone(),
                connected: network.connected,
            })
            .collect();
        if rows == self.heard {
            return false;
        }
        self.heard = rows;
        self.rebuild_menu();
        println!(
            "NETSTATUS:WIFI:NETWORKS count={} listed={}",
            self.heard.len(),
            self.menu
                .rows
                .iter()
                .filter(|row| (wifitray::menu::NETWORK_BASE..wifitray::menu::RESCAN - 1)
                    .contains(&row.id))
                .count()
        );
        true
    }

    /// Do what menu row `id` asks. The outcome reaches the item through the
    /// state topic; here it is only logged, and the next look at the state
    /// and the networks is brought forward.
    pub fn act(&mut self, id: u32, now: u64) {
        let action = self.menu.action(id);
        let acted = action != Action::None;
        match action {
            Action::None => {}
            Action::Connect { id } => {
                println!("NETSTATUS:WIFI:ACTION connect");
                report("CONNECT", wifi::connect(&self.ifname, &id));
            }
            Action::JoinOpen { ssid } => {
                println!("NETSTATUS:WIFI:ACTION join-open ssid={ssid}");
                let joined = wifi::add_network(&ssid, Security::Open, false, None)
                    .and_then(|id| wifi::connect(&self.ifname, &id));
                report("JOIN", joined);
            }
            Action::Configure => {
                println!("NETSTATUS:WIFI:ACTION configure");
                report("OPEN", wifi::open_network_app(NETWORK_WIFI_ARG));
            }
            Action::Disconnect => {
                println!("NETSTATUS:WIFI:ACTION disconnect");
                report("DISCONNECT", wifi::disconnect(&self.ifname));
            }
            Action::Rescan => {
                println!("NETSTATUS:WIFI:ACTION rescan");
                report("SCAN", wifi::scan(&self.ifname));
            }
            Action::Settings => {
                println!("NETSTATUS:WIFI:ACTION settings");
                report("OPEN", wifi::open_network_app(""));
            }
        }
        if acted {
            self.next_status = now;
            self.next_networks = now + 100;
        }
    }
}

fn report(what: &str, result: Result<impl Sized, wifi::Error>) {
    match result {
        Ok(_) => println!("NETSTATUS:WIFI:{what}:PASS"),
        Err(error) => println!("NETSTATUS:WIFI:{what}:FAIL {}", error.describe()),
    }
}
