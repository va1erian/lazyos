//! `xui-network`: the Network app. The top half shows what the network stack
//! (`netd`) reports, refreshed every second: the interface, how it is
//! configured, its address, the gateway and the traffic. The bottom half edits
//! the configuration `netd` reads from `confd` (`sys/net/<if>/*`, for the
//! interface shown; "Next card" steps through several): automatic
//! (DHCP) or a manual address, gateway and DNS server. `netd` re-reads it
//! every few seconds and rebuilds that interface to apply a change (its open
//! connections drop); "Renew lease" asks for a fresh DHCP lease at once.
//!
//! The form is checked with `netd`'s own parser before anything is written
//! (`xui_app::net::model::plan`), so a setup the stack would replace with DHCP
//! is refused with the reason instead of being saved.
//!
//! With a wireless interface (`wlanmd`, `LAZYOS_WIFI=1`) a **Wi-Fi** button
//! appears beside Next card and opens the Wi-Fi page in the same window: the
//! networks heard, connect (a password form for a new WPA2 network, "Other
//! network..." for a hidden one), disconnect and forget; see
//! `network/wifi_page.rs` and `network/wifi_join.rs`.
//!
//! Serial evidence: `NETAPP:UP:PASS` after the first frame;
//! `NETAPP:STATUS:PASS addr=<cidr> gw=<ip>` whenever the address changes
//! (`NETAPP:STATUS:NOSTACK` once when `netd` is absent);
//! `NETAPP:APPLY:PASS mode=<dhcp|static> if=<card>`, `NETAPP:APPLY:REFUSED` (the form) or
//! `NETAPP:APPLY:FAIL` (cancelled or not approved) on Apply;
//! `NETAPP:RENEW:PASS`/`FAIL` on Renew; `NETAPP:CLOSE:PASS` on close. The
//! Wi-Fi page prints `NETAPP:WIFI:*` lines (`wifi_page.rs`).

use xui_app::launch;
use xui_app::net::apply;
use xui_app::net::model::{self, Form, NetStatus};
use xui_app::net::stack;
use xui_app::platform::confd_store::ConfdStore;
use xui_app::platform::elevd;
use xui_core::app::{App, Ui};
use xui_core::backend::WidgetId;
use xui_core::prelude::*;
use xui_core::widget::{Dialog, DialogAction, RadioGroup};
use xui_settings::store::{ConfigStore, Value};

#[path = "network/wifi_actions.rs"]
mod wifi_actions;
#[path = "network/wifi_join.rs"]
mod wifi_join;
#[path = "network/wifi_page.rs"]
mod wifi_page;
#[path = "network/wifi_probe.rs"]
mod wifi_probe;

/// The window title, which the UI probe lines name.
const TITLE: &str = "Network";

/// The window size when a compositor lays the app out.
const WINDOW: (i32, i32) = (600, 470);
/// How often the status refreshes.
const REFRESH_MILLIS: u32 = 1000;

#[derive(Clone)]
enum Msg {
    Tick,
    Apply,
    Renew,
    Revert,
    NextCard,
    Close,
    /// The Wi-Fi page: open, back to the wired page, scan, and its buttons.
    WifiOpen,
    WifiBack,
    WifiScan,
    WifiConnect,
    WifiDisconnect,
    WifiForget,
    WifiOther,
    /// The Forget confirmation was answered (`true`: forget it).
    ForgetAnswer(bool),
    /// The join form: Cancel and Connect.
    JoinCancel,
    JoinOk,
}

/// The status rows: caption, then the line it shows.
const STATUS_ROWS: [&str; 5] = ["Interface", "Mode", "Address", "Gateway", "Traffic"];

/// Every widget the app reads or changes after start-up.
#[derive(Default)]
struct Widgets {
    values: [Handle<Label<Msg>>; 5],
    mode: Handle<RadioGroup<Msg>>,
    address: Handle<Edit<Msg>>,
    gateway: Handle<Edit<Msg>>,
    dns: Handle<Edit<Msg>>,
    message: Handle<Label<Msg>>,
    /// The wired page's two groups, hidden while the Wi-Fi page shows.
    status_group: Handle<GroupBox<Msg>>,
    config_group: Handle<GroupBox<Msg>>,
    /// Opens the Wi-Fi page; shown once a wireless interface exists.
    wifi_button: Handle<Button<Msg>>,
    wifi: wifi_page::WifiWidgets,
}

impl Widgets {
    /// The Status group over the Configuration group and the message line.
    /// The sizes keep the options and the buttons where the `net_config`
    /// session clicks.
    fn layout(&self) -> Layout<Msg> {
        let mut status = Vec::new();
        for (caption, value) in STATUS_ROWS.iter().zip(&self.values) {
            status.push(label(*caption).into_entry());
            status.push(label("...").bind(value).into_entry());
        }
        let mut fields = Vec::new();
        for (caption, handle, cue) in [
            ("Address / prefix", &self.address, "192.168.1.20/24"),
            ("Gateway", &self.gateway, "192.168.1.1 (optional)"),
            ("DNS server", &self.dns, "10.0.2.3 (optional)"),
        ] {
            fields.push(
                row()
                    .gap(8)
                    .children((
                        label(caption).width(132).align(Align::Center),
                        edit().placeholder(cue).bind(handle).width(200),
                    ))
                    .into_entry(),
            );
        }
        column()
            .padding(Insets::new(Dip(12.0), Dip(8.0), Dip(12.0), Dip(8.0)))
            .gap(8)
            .children((
                group(
                    "Status",
                    grid([Track::Fixed(Dip(100.0)), Track::Fill(1)]).children(status),
                )
                .bind(&self.status_group),
                group(
                    "Configuration",
                    column().gap(8).children((
                        radio_group(&["Automatic (DHCP)", "Manual (static address)"])
                            .bind(&self.mode),
                        row().gap(12).children((
                            column().gap(8).children(fields),
                            label("Used in Manual mode.").align(Align::Start),
                        )),
                        row().gap(8).children((
                            button("Apply").on_click(Msg::Apply).width(90),
                            button("Renew lease").on_click(Msg::Renew).width(120),
                            button("Revert").on_click(Msg::Revert).width(90),
                            button("Next card").on_click(Msg::NextCard).width(100),
                            button("Wi-Fi")
                                .on_click(Msg::WifiOpen)
                                .bind(&self.wifi_button)
                                .width(80),
                        )),
                    )),
                )
                .bind(&self.config_group),
                label("").bind(&self.message).fixed(44),
                self.wifi.page_layout(),
                self.wifi.known_layout(),
                self.wifi.other_layout(),
            ))
    }

    /// The wired page's widgets, shown and hidden together.
    fn wired_ids(&self) -> [WidgetId; 3] {
        [
            self.status_group.get().id(),
            self.config_group.get().id(),
            self.message.get().id(),
        ]
    }
}

struct Network {
    w: Widgets,
    ui: Ui<Msg>,
    wifi: wifi_page::Wifi,
    store: ConfdStore,
    status: Option<NetStatus>,
    /// The interface being shown and edited, by name; `None` follows the
    /// primary one.
    selected: Option<String>,
    /// The address last reported on serial, so a change is reported once.
    reported: Option<String>,
    /// The card the form was filled from. Apply writes to this card and only
    /// while it is still the one shown: it waits for the first status (so
    /// Manual starts from the live address) and never defaults to a card.
    form_card: Option<String>,
}

impl App for Network {
    type Msg = Msg;

    fn update(&mut self, msg: Msg, ui: &mut Ui<Msg>) {
        match msg {
            Msg::Tick => {
                self.refresh();
                self.wifi_tick();
            }
            Msg::Apply => self.apply(),
            Msg::Renew => self.renew(),
            Msg::Revert => {
                self.fill();
                self.w
                    .message
                    .get()
                    .set_text("Form reset to the saved configuration.");
            }
            Msg::NextCard => self.next_card(),
            Msg::WifiOpen => self.wifi_open(),
            Msg::WifiBack => self.wifi_back(),
            Msg::WifiScan => self.wifi_scan_now(),
            Msg::WifiConnect => self.wifi_connect(),
            Msg::WifiDisconnect => self.wifi_disconnect(),
            Msg::WifiForget => self.wifi_forget_ask(),
            Msg::WifiOther => self.wifi_other(),
            Msg::ForgetAnswer(yes) => self.wifi_forget_answer(yes),
            Msg::JoinCancel => self.join_cancel(),
            Msg::JoinOk => self.join_submit(),
            Msg::Close => {
                println!("NETAPP:CLOSE:PASS");
                ui.quit();
            }
        }
    }
}

impl Network {
    /// Read the stack and show it; fill the form once the first answer is in.
    fn refresh(&mut self) {
        match stack::status(self.selected.as_deref()) {
            Ok(status) => {
                let lines = [
                    status.interface_line(),
                    status.mode_line(),
                    status.address_line(),
                    status.gateway_line(),
                    status.traffic_line(),
                ];
                for (label, line) in self.w.values.iter().zip(lines) {
                    label.get().set_text(&line);
                }
                let cidr = status.cidr();
                if cidr.is_some() && cidr != self.reported {
                    let gw = status
                        .gateway
                        .map_or_else(|| String::from("none"), model::dotted);
                    println!(
                        "NETAPP:STATUS:PASS addr={} gw={gw}",
                        cidr.as_deref().unwrap_or("")
                    );
                    self.reported = cidr;
                }
                self.status = Some(status);
            }
            Err(error) => {
                if self.status.is_some() || self.form_card.is_none() {
                    println!("NETAPP:STATUS:NOSTACK {}", error.describe());
                }
                self.w.values[0].get().set_text(&error.describe());
                for label in &self.w.values[1..] {
                    label.get().set_text("-");
                }
                self.status = None;
            }
        }
        // The shown card changed under the form (the primary moved, or the
        // first answer came): the form is refilled from the new card, never
        // left holding another card's values.
        if let Some(shown) = self.shown_card() {
            if self.form_card.as_deref() != Some(shown.as_str()) {
                let first = self.form_card.is_none();
                self.fill();
                if !first {
                    self.w.message.get().set_text(&format!(
                        "The card shown is now {shown}; the form was reloaded from it."
                    ));
                }
            }
        }
    }

    /// The interface the status shows, if the stack answered.
    fn shown_card(&self) -> Option<String> {
        self.status
            .as_ref()
            .and_then(|status| status.interface.as_ref())
            .map(|i| i.name.clone())
    }

    /// Show the next interface `netd` drives and load its configuration.
    fn next_card(&mut self) {
        let Some(status) = self.status.as_ref() else {
            return;
        };
        let current = status.interface.as_ref().map(|i| i.name.clone());
        let Some(next) = model::next_name(&status.names, current.as_deref()).map(String::from)
        else {
            return;
        };
        self.selected = Some(next);
        self.reported = None;
        self.refresh();
        self.fill();
        let name = self.shown_card().unwrap_or_default();
        println!("NETAPP:CARD:PASS if={name}");
        self.w
            .message
            .get()
            .set_text(&format!("Showing {name}. Apply saves to this card only."));
    }

    /// Put the saved configuration (or the live address) into the form.
    fn fill(&mut self) {
        let Some(ifname) = self.shown_card() else {
            return;
        };
        let stored = ["mode", "address", "gateway", "dns"].map(|name| {
            match self.store.get(&model::key(&ifname, name)) {
                Some(Value::Str(text)) => Some(text),
                _ => None,
            }
        });
        let form = model::form_from(
            stored,
            self.status.as_ref().unwrap_or(&NetStatus::default()),
        );
        self.w.mode.get().select(usize::from(form.manual));
        self.w.address.get().set_text(&form.address);
        self.w.gateway.get().set_text(&form.gateway);
        self.w.dns.get().set_text(&form.dns);
        self.form_card = Some(ifname);
    }

    fn form(&self) -> Form {
        Form {
            manual: self.w.mode.get().selected() == 1,
            address: self.w.address.get().text(),
            gateway: self.w.gateway.get().text(),
            dns: self.w.dns.get().text(),
        }
    }

    /// Check the form and hand it to `elevd`: `sys/net/**` is a system
    /// setting, so the compositor asks an administrator to approve the
    /// change to this card (one prompt for all of its keys).
    fn apply(&self) {
        let form = self.form();
        // The card the form was filled from, confirmed as still the one
        // shown; anything else is refused rather than aimed at a default.
        let ifname = match (self.form_card.clone(), self.shown_card()) {
            (Some(filled), Some(shown)) if filled == shown => filled,
            _ => {
                println!("NETAPP:APPLY:REFUSED");
                self.w.message.get().set_text(
                    "The card this form edits cannot be confirmed (the network service did not answer, or the card changed). Nothing was saved.",
                );
                return;
            }
        };
        let args = match apply::elevd_args(&ifname, &form) {
            Ok(args) => args,
            Err(why) => {
                println!("NETAPP:APPLY:REFUSED");
                self.w.message.get().set_text(&why);
                return;
            }
        };
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        if let Err(error) = elevd::request("net.config", &args) {
            println!("NETAPP:APPLY:FAIL");
            self.w
                .message
                .get()
                .set_text(&format!("Could not save: {}", elevd::describe(&error)));
            return;
        }
        let mode = if form.manual { "static" } else { "dhcp" };
        println!("NETAPP:APPLY:PASS mode={mode} if={ifname}");
        self.w.message.get().set_text(if form.manual {
            "Saved. This card restarts with the manual address within a few seconds."
        } else {
            "Saved. This card restarts with DHCP within a few seconds."
        });
    }

    fn renew(&self) {
        match stack::renew() {
            Ok(()) => {
                println!("NETAPP:RENEW:PASS");
                self.w
                    .message
                    .get()
                    .set_text("Asked the DHCP server for a fresh lease.");
            }
            Err(error) => {
                println!("NETAPP:RENEW:FAIL");
                self.w
                    .message
                    .get()
                    .set_text(&format!("Renew failed: {}", error.describe()));
            }
        }
    }
}

fn main() {
    launch::run("NETAPP", "Network", WINDOW, |ui, backend| {
        backend.on_first_frame(|| println!("NETAPP:UP:PASS"));
        let w = Widgets::default();
        ui.root(w.layout())
            .inspect_err(|error| println!("NETAPP:BUILD:FAIL:{error}"))?;
        ui.every(REFRESH_MILLIS, Msg::Tick);
        ui.on_close(|| Some(Msg::Close));
        let forget = Dialog::confirm(ui, "Forget this network?", "")?
            .accept_label("Forget")
            .on_action(|action| Some(Msg::ForgetAnswer(matches!(action, DialogAction::Accept(_)))));
        // The Wi-Fi pages and the Wi-Fi button wait for a wireless interface.
        wifi_page::start_hidden(ui, &w);
        let mut app = Network {
            w,
            ui: ui.clone(),
            wifi: wifi_page::Wifi::new(forget),
            store: ConfdStore::new(),
            status: None,
            selected: None,
            reported: None,
            form_card: None,
        };
        app.refresh();
        Ok(app)
    })
}
