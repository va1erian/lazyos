//! `xui-network`: the Network app. The top half shows what the network stack
//! (`netd`) reports, refreshed every second: the interface, how it is
//! configured, its address, the gateway and the traffic. The bottom half edits
//! the configuration `netd` reads from `confd` (`sys/net/<if>/*`, for the
//! interface shown; "Next card" steps through several): automatic
//! (DHCP) or a manual address, gateway and DNS server. `netd` re-reads it
//! every few seconds and restarts itself to apply a change (open connections
//! drop); "Renew lease" asks for a fresh DHCP lease at once.
//!
//! The form is checked with `netd`'s own parser before anything is written
//! (`xui_app::net::model::plan`), so a setup the stack would replace with DHCP
//! is refused with the reason instead of being saved.
//!
//! Serial evidence: `NETAPP:UP:PASS` after the first frame;
//! `NETAPP:STATUS:PASS addr=<cidr> gw=<ip>` whenever the address changes
//! (`NETAPP:STATUS:NOSTACK` once when `netd` is absent);
//! `NETAPP:APPLY:PASS mode=<dhcp|static>` or `NETAPP:APPLY:REFUSED` on Apply;
//! `NETAPP:RENEW:PASS`/`FAIL` on Renew; `NETAPP:CLOSE:PASS` on close.

use xui_app::launch;
use xui_app::net::model::{self, Form, NetStatus, Write};
use xui_app::net::stack;
use xui_app::platform::confd_store::ConfdStore;
use xui_confd_editor::store::StoreError as ConfStoreError;
use xui_core::app::{App, Ui};
use xui_core::prelude::*;
use xui_core::widget::RadioGroup;
use xui_settings::store::{ConfigStore, Value};

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
                ),
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
                        )),
                    )),
                ),
                label("").bind(&self.message).fixed(44),
            ))
    }
}

struct Network {
    w: Widgets,
    store: ConfdStore,
    status: Option<NetStatus>,
    /// The interface being shown and edited, by name; `None` follows the
    /// primary one.
    selected: Option<String>,
    /// The address last reported on serial, so a change is reported once.
    reported: Option<String>,
    /// Whether the form has been filled (it waits for the first status, so
    /// Manual starts from the live address).
    filled: bool,
}

impl App for Network {
    type Msg = Msg;

    fn update(&mut self, msg: Msg, ui: &mut Ui<Msg>) {
        match msg {
            Msg::Tick => self.refresh(),
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
                if self.status.is_some() || !self.filled {
                    println!("NETAPP:STATUS:NOSTACK {}", error.describe());
                }
                self.w.values[0].get().set_text(&error.describe());
                for label in &self.w.values[1..] {
                    label.get().set_text("-");
                }
                self.status = None;
            }
        }
        if !self.filled {
            self.fill();
            self.filled = true;
        }
    }

    /// The interface the form edits.
    fn ifname(&self) -> String {
        self.status
            .as_ref()
            .and_then(|status| status.interface.as_ref())
            .map_or_else(|| String::from(model::DEFAULT_IFNAME), |i| i.name.clone())
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
        let name = self.ifname();
        println!("NETAPP:CARD:PASS if={name}");
        self.w
            .message
            .get()
            .set_text(&format!("Showing {name}. Apply saves to this card only."));
    }

    /// Put the saved configuration (or the live address) into the form.
    fn fill(&self) {
        let ifname = self.ifname();
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
    }

    fn form(&self) -> Form {
        Form {
            manual: self.w.mode.get().selected() == 1,
            address: self.w.address.get().text(),
            gateway: self.w.gateway.get().text(),
            dns: self.w.dns.get().text(),
        }
    }

    /// Check the form and write it to `confd`.
    fn apply(&self) {
        let form = self.form();
        let ifname = self.ifname();
        let writes = match model::plan(&ifname, &form) {
            Ok(writes) => writes,
            Err(why) => {
                println!("NETAPP:APPLY:REFUSED");
                self.w.message.get().set_text(&why);
                return;
            }
        };
        for write in &writes {
            let outcome = match write {
                Write::Set(key, value) => self.store.set(key, Value::Str(value.clone())),
                // Clearing a value that was never set is fine; any other
                // failure would leave a stale gateway or DNS server behind.
                Write::Delete(key) => {
                    match xui_confd_editor::store::ConfStore::delete(&self.store, key) {
                        Ok(()) | Err(ConfStoreError::NotFound) => Ok(()),
                        Err(error) => Err(error.message()),
                    }
                }
            };
            if let Err(error) = outcome {
                println!("NETAPP:APPLY:FAIL");
                self.w
                    .message
                    .get()
                    .set_text(&format!("Could not save: {error}"));
                return;
            }
        }
        let mode = if form.manual { "static" } else { "dhcp" };
        println!("NETAPP:APPLY:PASS mode={mode} if={ifname}");
        self.w.message.get().set_text(if form.manual {
            "Saved. The network stack restarts with the manual address within a few seconds."
        } else {
            "Saved. The network stack restarts with DHCP within a few seconds."
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
        let mut app = Network {
            w,
            store: ConfdStore::new(),
            status: None,
            selected: None,
            reported: None,
            filled: false,
        };
        app.refresh();
        Ok(app)
    })
}
