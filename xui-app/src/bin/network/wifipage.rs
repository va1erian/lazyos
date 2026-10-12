//! The Network app's Wi-Fi page: the networks `wlanmd` hears (signal,
//! security, saved or connected), Scan, Join (typing the passphrase of a new
//! WPA2 network), Disconnect, Forget, and a box to join a network that does
//! not announce its name. The tab exists whenever `wlanmd` is running (it adopts
//! a radio that attaches later), so an image without Wi-Fi keeps the
//! Ethernet-only window.
//!
//! Everything is the user's own: a network joined here is saved for the
//! signed-in user (`AddNetwork` scope `User`); the passphrase goes to `wlanmd`
//! in that one call, straight on to `keyd`, and the field is emptied at once.
//! Networks for every user need an administrator (`elevd`, `net.wifi.system`)
//! and are not offered here.
//!
//! Serial evidence (never a passphrase): `NETAPP:WIFI:UP if=<card>`,
//! `NETAPP:WIFI:STATE if=<card> state=<state> ssid="<name>"` on a change,
//! `NETAPP:WIFI:SCAN:PASS n=<networks>` when the list changes,
//! `NETAPP:WIFI:JOIN:REQUESTED id=<id> n=<k>`, `NETAPP:WIFI:JOIN:REFUSED <why>`,
//! `NETAPP:WIFI:LEAVE:PASS n=<k>`, `NETAPP:WIFI:FORGET:PASS id=<id> n=<k>`
//! (`k` counts joins, leaves and forgets).

use xui_app::net::wifi;
use xui_app::net::wifimodel::{self as model, Conn, Network, Security, State};
use xui_core::backend::WidgetId;
use xui_core::prelude::*;

use crate::Msg;

/// Ticks (seconds) between scans while not connected: a scan on a joined
/// radio pauses its traffic, so a connected station scans only when asked.
const RESCAN_TICKS: u32 = 20;

/// What the page's buttons ask for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Scan,
    Join,
    JoinHidden,
    Leave,
    Forget,
}

/// Every widget the page reads or changes after start-up.
#[derive(Default)]
pub struct Widgets {
    heading: Handle<Label<Msg>>,
    list: Handle<ListView<Msg>>,
    passphrase: Handle<Edit<Msg>>,
    hidden: Handle<Edit<Msg>>,
    scan: Handle<Button<Msg>>,
    join: Handle<Button<Msg>>,
    leave: Handle<Button<Msg>>,
    forget: Handle<Button<Msg>>,
    join_hidden: Handle<Button<Msg>>,
    message: Handle<Label<Msg>>,
}

impl Widgets {
    fn layout(&self) -> Layout<Msg> {
        column()
            .padding(Insets::new(Dip(12.0), Dip(8.0), Dip(12.0), Dip(8.0)))
            .gap(8)
            .children((
                label("Looking for a Wi-Fi radio...")
                    .bind(&self.heading)
                    .fixed(24),
                list()
                    .column("Network", 220)
                    .column("Signal", 130)
                    .column("Security", 90)
                    .column("State", Fill)
                    .on_activate(|_| Msg::Wifi(Action::Join))
                    .bind(&self.list)
                    .fill(1),
                row().gap(8).children((
                    label("Passphrase").width(80).align(Align::Center),
                    edit()
                        .password()
                        .placeholder("for a new WPA2 network")
                        .bind(&self.passphrase)
                        .width(220),
                    button("Join")
                        .on_click(Msg::Wifi(Action::Join))
                        .bind(&self.join)
                        .width(80),
                    button("Disconnect")
                        .on_click(Msg::Wifi(Action::Leave))
                        .bind(&self.leave)
                        .width(100),
                )),
                row().gap(8).children((
                    button("Scan")
                        .on_click(Msg::Wifi(Action::Scan))
                        .bind(&self.scan)
                        .width(80),
                    button("Forget")
                        .on_click(Msg::Wifi(Action::Forget))
                        .bind(&self.forget)
                        .width(80),
                )),
                group(
                    "Hidden network",
                    row().gap(8).children((
                        label("Name").width(80).align(Align::Center),
                        edit()
                            .placeholder("the network's name")
                            .bind(&self.hidden)
                            .width(220),
                        button("Join hidden")
                            .on_click(Msg::Wifi(Action::JoinHidden))
                            .bind(&self.join_hidden)
                            .width(110),
                        label("Passphrase above; empty is open.")
                            .align(Align::Start),
                    )),
                ),
                label("").bind(&self.message).fixed(44),
            ))
    }
}

pub struct Page {
    w: Widgets,
    /// The radio's interface, once `wlanmd` manages one (it may attach
    /// after the session starts).
    ifname: Option<String>,
    /// The table as last listed, in row order.
    networks: Vec<Network>,
    state: State,
    /// The state last reported on serial, so a change is reported once.
    reported: Option<(Conn, String)>,
    /// The number of networks last reported on serial.
    reported_count: Option<usize>,
    ticks: u32,
    /// Joins, leaves and forgets so far; the serial markers carry it so a
    /// session can wait for the Nth one.
    actions: u32,
}

impl Page {
    /// The page, or `None` when there is no `wlanmd` (no tab is shown then).
    /// A radio that attaches later is picked up by [`Page::tick`].
    pub fn detect() -> Option<Page> {
        if !wifi::available() {
            return None;
        }
        Some(Page {
            w: Widgets::default(),
            ifname: None,
            networks: Vec::new(),
            state: State::default(),
            reported: None,
            reported_count: None,
            ticks: 0,
            actions: 0,
        })
    }

    pub fn layout(&self) -> Layout<Msg> {
        self.w.layout()
    }

    /// The widgets the UI probe names (probe images only).
    pub fn probe_targets(&self) -> [(&'static str, WidgetId); 8] {
        [
            ("wifi_networks", self.w.list.get().id()),
            ("wifi_passphrase", self.w.passphrase.get().id()),
            ("wifi_join", self.w.join.get().id()),
            ("wifi_leave", self.w.leave.get().id()),
            ("wifi_scan", self.w.scan.get().id()),
            ("wifi_forget", self.w.forget.get().id()),
            ("wifi_hidden", self.w.hidden.get().id()),
            ("wifi_join_hidden", self.w.join_hidden.get().id()),
        ]
    }

    /// Start-up: the first look, and a scan unless already connected.
    pub fn start(&mut self) {
        self.refresh();
        if self.state.conn != Conn::Connected {
            self.scan(false);
        }
    }

    /// One tick (a second): the state and the list, a rescan now and then.
    pub fn tick(&mut self) {
        self.ticks = self.ticks.wrapping_add(1);
        if self.ticks % RESCAN_TICKS == 0
            && !self.state.busy()
            && self.state.conn != Conn::Connected
        {
            self.scan(false);
        }
        self.refresh();
    }

    pub fn act(&mut self, action: Action) {
        match action {
            Action::Scan => self.scan(true),
            Action::Join => self.join(),
            Action::JoinHidden => self.join_hidden(),
            Action::Leave => self.leave(),
            Action::Forget => self.forget(),
        }
    }

    fn say(&self, text: &str) {
        self.w.message.get().set_text(text);
    }

    /// Read the state and the table; keep the selection on the same network.
    fn refresh(&mut self) {
        if self.ifname.is_none() {
            match wifi::interfaces() {
                Ok(names) => {
                    self.ifname = names.into_iter().next();
                    if let Some(name) = &self.ifname {
                        println!("NETAPP:WIFI:UP if={name}");
                    }
                }
                Err(error) => {
                    self.w.heading.get().set_text(&error.describe());
                    return;
                }
            }
        }
        let Some(card) = self.ifname.clone() else {
            self.w.heading.get().set_text("No Wi-Fi radio found yet...");
            return;
        };
        match wifi::status(&card) {
            Ok(state) => {
                self.w
                    .heading
                    .get()
                    .set_text(&model::state_line(&card, &state));
                self.report(&state);
                self.state = state;
            }
            Err(error) => {
                self.w.heading.get().set_text(&error.describe());
                self.state = State::default();
                return;
            }
        }
        let Ok(networks) = wifi::networks(&card) else {
            return;
        };
        let selected = self.selected().map(|n| (n.ssid.clone(), n.id.clone()));
        self.w
            .list
            .get()
            .refresh_model(networks.iter().map(model::row).collect::<Vec<_>>());
        let index = selected
            .and_then(|(ssid, id)| {
                networks
                    .iter()
                    .position(|n| n.ssid == ssid && n.id == id)
                    .or_else(|| networks.iter().position(|n| n.ssid == ssid))
            })
            .or((!networks.is_empty()).then_some(0));
        self.w.list.get().select(index);
        if self.reported_count != Some(networks.len()) {
            println!("NETAPP:WIFI:SCAN:PASS n={}", networks.len());
            self.reported_count = Some(networks.len());
        }
        self.networks = networks;
    }

    fn report(&mut self, state: &State) {
        let now = (state.conn, state.ssid.clone());
        if self.reported.as_ref() == Some(&now) {
            return;
        }
        println!(
            "NETAPP:WIFI:STATE if={} state={} ssid=\"{}\"",
            self.ifname.as_deref().unwrap_or(""),
            state.conn.name(),
            state.ssid
        );
        self.reported = Some(now);
    }

    fn selected(&self) -> Option<&Network> {
        self.w
            .list
            .get()
            .selected()
            .and_then(|index| self.networks.get(index))
    }

    /// The radio's interface, or a message saying there is none yet.
    fn card(&self) -> Option<String> {
        let card = self.ifname.clone();
        if card.is_none() {
            self.say("There is no Wi-Fi radio yet.");
        }
        card
    }

    fn scan(&mut self, asked: bool) {
        let Some(card) = self.ifname.clone() else {
            return;
        };
        match wifi::scan(&card, &[]) {
            Ok(()) => {
                if asked {
                    self.say("Scanning...");
                }
            }
            // One already runs, or the last was a second ago: the list is
            // about to change anyway.
            Err(error) if error.is_busy() => {}
            Err(error) => {
                if asked {
                    self.say(&format!("Scan failed: {}", error.describe()));
                }
            }
        }
    }

    /// The passphrase typed, taken out of the field at once.
    fn take_passphrase(&self) -> String {
        let field = self.w.passphrase.get();
        let text = field.text();
        field.set_text("");
        text
    }

    fn refuse(&self, why: &str) {
        println!("NETAPP:WIFI:JOIN:REFUSED {why}");
        self.say(why);
    }

    /// Join the selected network: a saved one by its id; a new one after
    /// saving it for this user (with the typed passphrase when it is WPA2).
    fn join(&mut self) {
        let Some(network) = self.selected().cloned() else {
            self.refuse("Select a network first.");
            return;
        };
        if network.connected {
            self.say(&format!("Already connected to \"{}\".", network.ssid));
            return;
        }
        if network.known() {
            self.take_passphrase();
            self.start_join(&network.id, network.display_name());
            return;
        }
        if network.ssid.is_empty() {
            self.refuse("This network does not announce its name: use the Hidden network box.");
            return;
        }
        if !network.security.joinable() {
            self.refuse("LazyOS cannot join this kind of network yet (WPA2 and open ones only).");
            return;
        }
        let passphrase = if network.security.needs_passphrase() {
            let typed = self.take_passphrase();
            if typed.is_empty() {
                self.refuse(&format!(
                    "\"{}\" is protected: type its passphrase, then Join.",
                    network.ssid
                ));
                return;
            }
            if let Err(why) = model::check_passphrase(&typed) {
                self.refuse(&why);
                return;
            }
            Some(typed)
        } else {
            self.take_passphrase();
            None
        };
        self.save_and_join(&network.ssid, network.security, false, passphrase);
    }

    /// Join a network that does not announce itself: the typed name, WPA2
    /// with the typed passphrase, or open when the passphrase is empty.
    fn join_hidden(&mut self) {
        let ssid = self.w.hidden.get().text();
        if let Err(why) = model::check_ssid(&ssid) {
            self.refuse(&why);
            return;
        }
        let typed = self.take_passphrase();
        let (security, passphrase) = if typed.is_empty() {
            (Security::Open, None)
        } else if let Err(why) = model::check_passphrase(&typed) {
            self.refuse(&why);
            return;
        } else {
            (Security::Wpa2Psk, Some(typed))
        };
        // Probe for the name so the scan lists it, then save and join.
        if let Some(card) = self.ifname.clone() {
            let _ = wifi::scan(&card, std::slice::from_ref(&ssid));
        }
        self.save_and_join(&ssid, security, true, passphrase);
    }

    fn save_and_join(
        &mut self,
        ssid: &str,
        security: Security,
        hidden: bool,
        passphrase: Option<String>,
    ) {
        match wifi::add_network(ssid, security, hidden, passphrase) {
            Ok(id) => self.start_join(&id, ssid),
            Err(error) => self.refuse(&format!("Could not save \"{ssid}\": {}", error.describe())),
        }
    }

    fn start_join(&mut self, id: &str, name: &str) {
        let Some(card) = self.card() else {
            return;
        };
        match wifi::connect(&card, id) {
            Ok(()) => {
                self.actions += 1;
                println!("NETAPP:WIFI:JOIN:REQUESTED id={id} n={}", self.actions);
                self.say(&format!("Joining \"{name}\"..."));
                self.refresh();
            }
            Err(error) => self.refuse(&format!("Could not join \"{name}\": {}", error.describe())),
        }
    }

    fn leave(&mut self) {
        let Some(card) = self.card() else {
            return;
        };
        match wifi::disconnect(&card) {
            Ok(()) => {
                self.actions += 1;
                println!("NETAPP:WIFI:LEAVE:PASS n={}", self.actions);
                self.say("Disconnected. It stays off until you join a network.");
                self.refresh();
            }
            Err(error) => self.say(&format!("Could not disconnect: {}", error.describe())),
        }
    }

    fn forget(&mut self) {
        let Some(network) = self.selected().cloned() else {
            self.say("Select a saved network first.");
            return;
        };
        if !network.known() {
            self.say("That network is not saved.");
            return;
        }
        match wifi::forget(&network.id) {
            Ok(()) => {
                self.actions += 1;
                println!(
                    "NETAPP:WIFI:FORGET:PASS id={} n={}",
                    network.id, self.actions
                );
                self.say(&format!("Forgot \"{}\".", network.display_name()));
                self.refresh();
            }
            Err(error) => self.say(&format!("Could not forget it: {}", error.describe())),
        }
    }
}
