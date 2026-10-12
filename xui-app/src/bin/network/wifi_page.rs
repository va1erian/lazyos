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
use xui_app::net::wifiview::{self, ScanPacer};
use xui_app::probe;
use xui_core::app::Ui;
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
    pub(super) page: Handle<GroupBox<Msg>>,
    pub(super) status: Handle<Label<Msg>>,
    pub(super) list: Handle<ListView<Msg>>,
    pub(super) connect: Handle<Button<Msg>>,
    pub(super) disconnect: Handle<Button<Msg>>,
    pub(super) forget: Handle<Button<Msg>>,
    pub(super) scan: Handle<Button<Msg>>,
    pub(super) other: Handle<Button<Msg>>,
    pub(super) back: Handle<Button<Msg>>,
    pub(super) message: Handle<Label<Msg>>,
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

    pub(super) fn known_layout(&self) -> impl IntoEntry<Msg> {
        self.join.known_layout()
    }

    pub(super) fn other_layout(&self) -> impl IntoEntry<Msg> {
        self.join.other_layout()
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
    pub(super) networks: Vec<Heard>,
    pub(super) status: Option<Status>,
    pub(super) pacer: ScanPacer,
    pub(super) ticks: u32,
    /// The state last reported on serial, so a change is reported once.
    pub(super) reported: Option<(ConnState, String)>,
    /// The Forget confirmation, and the network it is about.
    pub(super) forget_dialog: Dialog<Msg>,
    pub(super) forget_target: Option<(String, String)>,
    /// The probe lines wait for a frame with the page in place.
    pub(super) probe_pending: bool,
    /// The Wi-Fi button just appeared: probe it at the next frame.
    pub(super) button_probe: bool,
    pub(super) probed_rows: Vec<String>,
    pub(super) join: Option<wifiview::JoinKind>,
    /// A saved network that `Connect` did not know yet: asked again each tick.
    pub(super) pending: Option<Pending>,
    /// Started as `network /wifi` (the tray's Wi-Fi menu): open the page as
    /// soon as the radio is found.
    pub(super) open_when_found: bool,
}

/// A join to retry: the profile id, its name and the tries left.
pub(super) struct Pending {
    pub(super) id: String,
    pub(super) ssid: String,
    pub(super) left: u32,
}

/// How many ticks a join waits for `wlanmd` to learn a network just saved.
pub(super) const JOIN_RETRIES: u32 = 15;

impl Wifi {
    pub(super) fn new(forget_dialog: Dialog<Msg>, open_when_found: bool) -> Wifi {
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
            button_probe: false,
            probed_rows: Vec::new(),
            join: None,
            pending: None,
            open_when_found,
        }
    }
}

/// Hide the Wi-Fi pages and the Wi-Fi button until an interface is found.
pub(super) fn start_hidden(ui: &Ui<Msg>, w: &Widgets) {
    ui.set_visible(w.wifi.page.get().id(), false);
    w.wifi.join.show(ui, None);
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
            if std::mem::take(&mut self.wifi.button_probe) {
                let r = self.ui.bounds(self.w.wifi_button.get().id());
                probe::widget(
                    TITLE,
                    "wifi_button",
                    r.left,
                    r.top,
                    r.right - r.left,
                    r.bottom - r.top,
                );
                println!("NETAPP:WIFI:PROBE:wired");
            }
            return;
        }
        self.wifi_poll();
        self.wifi_retry_join();
        if self.wifi.probe_pending {
            self.wifi.probe_pending = false;
            self.wifi_probe();
        }
    }

    /// Look for a wireless interface; the button appears when there is one.
    pub(super) fn wifi_detect(&mut self) {
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
        self.wifi.button_probe = true;
        self.wifi.ifname = Some(ifname);
        if std::mem::take(&mut self.wifi.open_when_found) {
            self.wifi_open();
        }
    }

    /// Show one page of the window.
    pub(super) fn wifi_show(&mut self, view: View) {
        self.wifi.view = view;
        let ui = &self.ui;
        for id in self.w.wired_ids() {
            ui.set_visible(id, view == View::Wired);
        }
        ui.set_visible(self.w.wifi.page.get().id(), view == View::List);
        let kind = self.wifi.join.as_ref().filter(|_| view == View::Join);
        self.w.wifi.join.show(ui, kind);
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
        self.wifi.pending = None;
        self.wifi_show(View::Wired);
    }

    pub(super) fn wifi_say(&self, text: &str) {
        self.w.wifi.message.get().set_text(text);
    }

    /// The state, the list and, when due, a scan.
    pub(super) fn wifi_poll(&mut self) {
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

    pub(super) fn wifi_show_list(&mut self, list: Vec<Heard>) {
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
    pub(super) fn wifi_report(&mut self, status: &Status) {
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
            ConnState::Connected => {
                println!("NETAPP:WIFI:CONNECTED ssid={}", status.ssid);
                // "Connecting to ..." has come true: the status line says so.
                self.wifi_say("");
            }
            ConnState::Failed => {
                println!(
                    "NETAPP:WIFI:FAILED reason={} ssid={}",
                    wifi::reason_text(status.reason).unwrap_or("unknown"),
                    status.ssid
                );
                self.wifi_say("");
            }
            _ => {}
        }
        self.wifi.reported = Some(now);
    }

    /// The listed network under the selection.
    pub(super) fn selected_network(&self) -> Option<Heard> {
        let index = self.w.wifi.list.try_get()?.selected()?;
        self.wifi.networks.get(index).cloned()
    }
}
