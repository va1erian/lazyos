//! The Wi-Fi page of the Network app: the networks `wlanmd` heard, merged
//! with what is saved, and the buttons that act on one. The join form (a
//! password, or an "Other network") is `wifi_join.rs`.
//!
//! The page lives in the same window as the wired page: a **Wi-Fi** button
//! appears beside Next card once `wlanmd` reports a wireless interface, and
//! the page's groups and the wired ones are shown one at a time
//! (`Ui::set_visible`; a hidden group gives up its space, so the wired page
//! is laid out exactly as before). While the page is shown, the app asks
//! `wlanmd` for the state and the list once a second and for a fresh scan
//! every few seconds, never during a join. A busy answer (`EAGAIN`,
//! `EBUSY`) is not shown.
//!
//! Names are text: an SSID is any bytes in the air, and goes to a label or a
//! list cell, never into markup or a command. Serial evidence:
//! `NETAPP:WIFI:UP if=<card>`, `NETAPP:WIFI:OPEN`, `NETAPP:WIFI:LIST
//! count=<n>`, `NETAPP:WIFI:STATE <label> ssid=<name>`, `NETAPP:WIFI:CONNECT
//! ssid=<name>` (an attempt started), `NETAPP:WIFI:CONNECTED ssid=<name>`,
//! `NETAPP:WIFI:FAILED reason=<text> ssid=<name>`, `NETAPP:WIFI:FORGET
//! ssid=<name>`, `NETAPP:WIFI:PROBE:list|join` (the UI probe lines were
//! printed). The name is always the last field. No line carries a password.

use xui_app::net::wifi::{self, ConnState, Network as Heard, Status};
use xui_app::net::wifiview::{self, Plan, ScanPacer};
use xui_app::platform::elevd;
use xui_app::probe;
use xui_core::app::Ui;
use xui_core::backend::WidgetId;
use xui_core::prelude::*;
use xui_core::widget::{Dialog, ListModel};
use xui_core::{IconRef, Lucide};

use super::wifi_join::JoinWidgets;
use super::{Msg, Network, Widgets, TITLE};

/// How many ticks pass between two looks for a wireless interface.
const DETECT_EVERY: u32 = 3;

/// Which page of the window shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum View {
    Wired,
    List,
    Join,
}

/// Every widget of the Wi-Fi page and its join form.
#[derive(Default)]
pub(super) struct WifiWidgets {
    page: Handle<GroupBox<Msg>>,
    status: Handle<Label<Msg>>,
    list: Handle<ListView<Msg>>,
    connect: Handle<Button<Msg>>,
    disconnect: Handle<Button<Msg>>,
    forget: Handle<Button<Msg>>,
    scan: Handle<Button<Msg>>,
    other: Handle<Button<Msg>>,
    back: Handle<Button<Msg>>,
    message: Handle<Label<Msg>>,
    pub(super) join: JoinWidgets,
}

impl WifiWidgets {
    /// The page: the state line, the list, the buttons, the message line.
    pub(super) fn page_layout(&self) -> impl IntoEntry<Msg> {
        group(
            "Wi-Fi",
            column().gap(8).children((
                label("Looking for networks...").bind(&self.status),
                list()
                    .column("Network", 170)
                    .column("Security", 80)
                    .column("Signal", 120)
                    .column("Band", 70)
                    .column("State", Fill)
                    .on_activate(|_| Msg::WifiConnect)
                    .bind(&self.list)
                    .fill(1),
                row().gap(8).children((
                    button("Connect")
                        .on_click(Msg::WifiConnect)
                        .bind(&self.connect)
                        .width(76),
                    button("Disconnect")
                        .on_click(Msg::WifiDisconnect)
                        .bind(&self.disconnect)
                        .width(88),
                    button("Forget")
                        .on_click(Msg::WifiForget)
                        .bind(&self.forget)
                        .width(66),
                    button("Refresh")
                        .on_click(Msg::WifiScan)
                        .bind(&self.scan)
                        .width(72),
                    button("Other network...")
                        .on_click(Msg::WifiOther)
                        .bind(&self.other)
                        .width(122),
                    button("Back")
                        .on_click(Msg::WifiBack)
                        .bind(&self.back)
                        .width(64),
                )),
                label("").bind(&self.message).fixed(40),
            )),
        )
        .bind(&self.page)
        .fill(1)
    }

    pub(super) fn join_layout(&self) -> impl IntoEntry<Msg> {
        self.join.layout()
    }
}

/// The list's rows, with the lock icon on a network that asks for a password.
struct NetModel(Vec<wifiview::Row>);

impl ListModel for NetModel {
    fn rows(&self) -> usize {
        self.0.len()
    }

    fn cell(&self, row: usize, column: usize) -> Option<&str> {
        self.0.get(row)?.cells.get(column).map(String::as_str)
    }

    fn icon(&self, row: usize) -> Option<IconRef> {
        self.0
            .get(row)
            .is_some_and(|row| row.locked)
            .then(|| IconRef::from(Lucide::Lock))
    }
}

/// The Wi-Fi page's state; never a password.
pub(super) struct Wifi {
    pub(super) view: View,
    /// The wireless interface the page drives; `None` until one is found.
    pub(super) ifname: Option<String>,
    networks: Vec<Heard>,
    status: Option<Status>,
    pacer: ScanPacer,
    ticks: u32,
    /// The state last reported on serial, so a change is reported once.
    reported: Option<(ConnState, String)>,
    /// The Forget confirmation, and the network it is about.
    forget_dialog: Dialog<Msg>,
    forget_target: Option<(String, String)>,
    /// The probe lines wait for a frame with the page in place.
    pub(super) probe_pending: bool,
    probed_rows: Vec<String>,
    pub(super) join: Option<wifiview::JoinKind>,
}

impl Wifi {
    pub(super) fn new(forget_dialog: Dialog<Msg>) -> Wifi {
        Wifi {
            view: View::Wired,
            ifname: None,
            networks: Vec::new(),
            status: None,
            pacer: ScanPacer::default(),
            ticks: 0,
            reported: None,
            forget_dialog,
            forget_target: None,
            probe_pending: false,
            probed_rows: Vec::new(),
            join: None,
        }
    }
}

/// Hide the Wi-Fi pages and the Wi-Fi button until an interface is found.
pub(super) fn start_hidden(ui: &Ui<Msg>, w: &Widgets) {
    ui.set_visible(w.wifi.page.get().id(), false);
    ui.set_visible(w.wifi.join.group_id(), false);
    ui.set_visible(w.wifi_button.get().id(), false);
}

impl Network {
    /// One refresh tick: find the radio, then keep the page current.
    pub(super) fn wifi_tick(&mut self) {
        self.wifi.ticks = self.wifi.ticks.wrapping_add(1);
        if self.wifi.ifname.is_none() {
            if self.wifi.ticks % DETECT_EVERY == 1 {
                self.wifi_detect();
            }
            return;
        }
        if self.wifi.view == View::Wired {
            return;
        }
        self.wifi_poll();
        if self.wifi.probe_pending {
            self.wifi.probe_pending = false;
            self.wifi_probe();
        }
    }

    /// Look for a wireless interface; the button appears when there is one.
    fn wifi_detect(&mut self) {
        let Ok(names) = wifi::interfaces() else {
            return;
        };
        let shown = self.shown_card();
        let pick = names
            .iter()
            .find(|name| Some(name.as_str()) == shown.as_deref())
            .or(names.first())
            .cloned();
        let Some(ifname) = pick else {
            return;
        };
        println!("NETAPP:WIFI:UP if={ifname}");
        self.ui.set_visible(self.w.wifi_button.get().id(), true);
        self.wifi.ifname = Some(ifname);
    }

    /// Show one page of the window.
    pub(super) fn wifi_show(&mut self, view: View) {
        self.wifi.view = view;
        let ui = &self.ui;
        for id in self.w.wired_ids() {
            ui.set_visible(id, view == View::Wired);
        }
        ui.set_visible(self.w.wifi.page.get().id(), view == View::List);
        ui.set_visible(self.w.wifi.join.group_id(), view == View::Join);
        self.wifi.probe_pending = view != View::Wired;
    }

    pub(super) fn wifi_open(&mut self) {
        let Some(ifname) = self.wifi.ifname.clone() else {
            return;
        };
        println!("NETAPP:WIFI:OPEN if={ifname}");
        self.wifi.pacer.restart();
        self.wifi.reported = None;
        self.wifi_show(View::List);
        self.wifi_say("");
        self.wifi_poll();
    }

    pub(super) fn wifi_back(&mut self) {
        self.wifi_show(View::Wired);
    }

    pub(super) fn wifi_say(&self, text: &str) {
        self.w.wifi.message.get().set_text(text);
    }

    /// The state, the list and, when due, a scan.
    fn wifi_poll(&mut self) {
        let Some(ifname) = self.wifi.ifname.clone() else {
            return;
        };
        match wifi::status(&ifname) {
            Ok(status) => {
                self.w
                    .wifi
                    .status
                    .get()
                    .set_text(&wifiview::status_line(&status));
                self.wifi_report(&status);
                self.wifi.status = Some(status);
            }
            Err(error) if error.is_busy() => {}
            Err(error) => {
                self.w.wifi.status.get().set_text(&error.describe());
                self.wifi.status = None;
            }
        }
        let joining = self.wifi.status.as_ref().is_some_and(|s| s.state.joining());
        if self.wifi.pacer.tick(joining) {
            if let Err(error) = wifi::scan(&ifname) {
                if error.is_busy() {
                    self.wifi.pacer.retry_soon();
                }
            }
        }
        match wifi::networks(&ifname) {
            Ok(list) => self.wifi_show_list(list),
            Err(error) if error.is_busy() => {}
            Err(error) => self.wifi_say(&error.describe()),
        }
    }

    fn wifi_show_list(&mut self, list: Vec<Heard>) {
        let keep = self.selected_network().map(|n| n.ssid);
        let rows = wifiview::rows(&list);
        let names: Vec<String> = rows.iter().map(|r| r.cells[0].clone()).collect();
        let view = self.w.wifi.list.get();
        view.refresh_model(NetModel(rows));
        view.select(wifiview::selection_after(&list, keep.as_deref()));
        if list.len() != self.wifi.networks.len() {
            println!("NETAPP:WIFI:LIST count={}", list.len());
        }
        self.wifi.networks = list;
        if names != self.wifi.probed_rows {
            self.wifi.probed_rows = names;
            self.wifi.probe_pending = self.wifi.view == View::List;
        }
    }

    /// Report a change of state once.
    fn wifi_report(&mut self, status: &Status) {
        let now = (status.state, status.ssid.clone());
        if self.wifi.reported.as_ref() == Some(&now) {
            return;
        }
        println!(
            "NETAPP:WIFI:STATE {} ssid={}",
            status.state.label(),
            status.ssid
        );
        match status.state {
            ConnState::Connected => println!("NETAPP:WIFI:CONNECTED ssid={}", status.ssid),
            ConnState::Failed => println!(
                "NETAPP:WIFI:FAILED reason={} ssid={}",
                wifi::reason_text(status.reason).unwrap_or("unknown"),
                status.ssid
            ),
            _ => {}
        }
        self.wifi.reported = Some(now);
    }

    /// The listed network under the selection.
    pub(super) fn selected_network(&self) -> Option<Heard> {
        let index = self.w.wifi.list.try_get()?.selected()?;
        self.wifi.networks.get(index).cloned()
    }

    pub(super) fn wifi_scan_now(&mut self) {
        self.wifi.pacer.restart();
        self.wifi_say("Searching for networks...");
        self.wifi_poll();
    }

    /// Connect or Enter on a row: what the network needs decides.
    pub(super) fn wifi_connect(&mut self) {
        let Some(network) = self.selected_network() else {
            self.wifi_say("Select a network first.");
            return;
        };
        match wifiview::plan(&network) {
            Plan::Connect { id } => self.wifi_join_saved(&network.ssid, &id),
            Plan::AddOpen { ssid } => {
                match wifi::add_network(&ssid, wifi::Security::Open, false, None) {
                    Ok(id) => self.wifi_join_saved(&ssid, &id),
                    Err(error) => self.wifi_say(&format!("Could not save it: {}", error.describe())),
                }
            }
            Plan::AskPassword { ssid } => self.open_join(wifiview::JoinKind::Known { ssid }),
            Plan::Hidden => self.wifi_say("Use \"Other network...\" to join a hidden network."),
            Plan::Unsupported => self.wifi_say(
                "That network's security mode is not supported (open and WPA2 only).",
            ),
        }
    }

    /// Start joining the saved network `id`.
    pub(super) fn wifi_join_saved(&mut self, ssid: &str, id: &str) {
        let Some(ifname) = self.wifi.ifname.clone() else {
            return;
        };
        match wifi::connect(&ifname, id) {
            Ok(()) => {
                println!("NETAPP:WIFI:CONNECT ssid={ssid}");
                self.wifi_say(&format!("Connecting to {ssid}..."));
                self.wifi.pacer.restart();
            }
            Err(error) => self.wifi_say(&format!("Could not connect: {}", error.describe())),
        }
    }

    pub(super) fn wifi_disconnect(&mut self) {
        let Some(ifname) = self.wifi.ifname.clone() else {
            return;
        };
        match wifi::disconnect(&ifname) {
            Ok(()) => self.wifi_say("Disconnected. It stays off until you connect again."),
            Err(error) => self.wifi_say(&format!("Could not disconnect: {}", error.describe())),
        }
    }

    /// Forget asks first: the saved password goes with it.
    pub(super) fn wifi_forget_ask(&mut self) {
        let Some(network) = self.selected_network().filter(|n| !n.id.is_empty()) else {
            self.wifi_say("Select a saved network to forget.");
            return;
        };
        let dialog = &self.wifi.forget_dialog;
        dialog.set_message(&format!(
            "Forget \"{}\"? Its saved password is deleted. A network kept for all users needs an administrator to approve.",
            network.ssid
        ));
        self.wifi.forget_target = Some((network.id, network.ssid));
        self.wifi.forget_dialog.open();
    }

    pub(super) fn wifi_forget_answer(&mut self, yes: bool) {
        let Some((id, ssid)) = self.wifi.forget_target.take() else {
            return;
        };
        if !yes {
            return;
        }
        let result = match wifi::forget(&id) {
            // A network of all users is the administrator's to remove.
            Err(error) if error.is_denied() => elevd::request("net.wifi.forget", &[id.as_str()])
                .map(drop)
                .map_err(|error| elevd::describe(&error)),
            other => other.map_err(|error| error.describe()),
        };
        match result {
            Ok(()) => {
                println!("NETAPP:WIFI:FORGET ssid={ssid}");
                self.wifi_say(&format!("Forgot {ssid}."));
                self.wifi_poll();
            }
            Err(why) => self.wifi_say(&format!("Could not forget {ssid}: {why}")),
        }
    }

    /// Print where the page's controls and rows are (UI probe images only).
    pub(super) fn wifi_probe(&self) {
        let w = &self.w.wifi;
        let ui = &self.ui;
        let rect = |name: &str, id: WidgetId| {
            let r = ui.bounds(id);
            probe::widget(TITLE, name, r.left, r.top, r.right - r.left, r.bottom - r.top);
        };
        match self.wifi.view {
            View::Wired => return,
            View::List => {
                let buttons = [
                    ("wifi_connect", w.connect.get().id()),
                    ("wifi_disconnect", w.disconnect.get().id()),
                    ("wifi_forget", w.forget.get().id()),
                    ("wifi_scan", w.scan.get().id()),
                    ("wifi_other", w.other.get().id()),
                    ("wifi_back", w.back.get().id()),
                ];
                for (name, id) in buttons {
                    rect(name, id);
                }
                self.wifi_probe_rows();
                println!("NETAPP:WIFI:PROBE:list");
            }
            View::Join => {
                w.join.probe(ui, |name, id| rect(name, id));
                println!("NETAPP:WIFI:PROBE:join");
            }
        }
    }

    /// One `wifi_row:<name>` rectangle per listed network, from the list's
    /// header and row heights (24 and 22 design units).
    fn wifi_probe_rows(&self) {
        let r = self.ui.bounds(self.w.wifi.list.get().id());
        let dpi = self.ui.dpi() as i32;
        let (header, row) = (24 * dpi / 96, 22 * dpi / 96);
        for (index, name) in self.wifi.probed_rows.iter().enumerate() {
            let top = r.top + header + row * index as i32;
            probe::widget(
                TITLE,
                &format!("wifi_row:{name}"),
                r.left,
                top,
                r.right - r.left,
                row,
            );
        }
    }
}
