//! `xui-network`: the Network app. The top half shows what the network stack
//! (`netd`) reports, refreshed every second: the interface, how it is
//! configured, its address, the gateway and the traffic. The bottom half edits
//! the configuration `netd` reads from `confd` (`sys/net/eth0/*`): automatic
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
use xui_core::backend::WidgetId;
use xui_core::geometry::{Rect, Size};
use xui_core::layout::Constraints;
use xui_core::prelude::*;
use xui_core::widget::{Placeable, RadioGroup};
use xui_settings::store::{ConfigStore, Value};

/// The window size when a compositor lays the app out.
const WINDOW: (i32, i32) = (600, 470);
/// How often the status refreshes.
const REFRESH_MILLIS: u32 = 1000;
/// The design height of one radio option (`RadioGroup` draws them 28 tall).
const RADIO_ROW: Dip = Dip(28.0);

#[derive(Clone)]
enum Msg {
    Tick,
    Apply,
    Renew,
    Revert,
    Close,
}

/// The status rows: caption, then the line it shows.
const STATUS_ROWS: [&str; 5] = ["Interface", "Mode", "Address", "Gateway", "Traffic"];

/// The mode choice as a layout entry. A `RadioGroup` is one node per option
/// and not `Placeable`, so this stacks the options in the rectangle the layout
/// gives the first one.
struct ModeChoice {
    group: RadioGroup<Msg>,
    ids: Vec<WidgetId>,
}

impl ModeChoice {
    fn new(ui: &Ui<Msg>) -> xui_core::backend::Result<ModeChoice> {
        let group = RadioGroup::new(
            ui,
            Rect::default(),
            &["Automatic (DHCP)", "Manual (static address)"],
        )?;
        let ids = group.ids();
        Ok(ModeChoice { group, ids })
    }
}

impl Placeable<Msg> for ModeChoice {
    fn id(&self) -> WidgetId {
        self.ids[0]
    }

    fn measure(&self, _ui: &Ui<Msg>, constraints: Constraints) -> Size {
        let row = RADIO_ROW.to_px(constraints.dpi).value();
        Size::new(0, row * self.ids.len() as i32)
    }

    fn placed(&self, ui: &Ui<Msg>, rect: Rect) {
        let row = rect.height() / self.ids.len() as i32;
        let moves: Vec<(WidgetId, Rect)> = self
            .ids
            .iter()
            .enumerate()
            .map(|(index, id)| {
                let top = rect.top + row * index as i32;
                (*id, Rect::new(rect.left, top, rect.right, top + row))
            })
            .collect();
        ui.apply_moves(&moves);
    }
}

/// Every widget the app reads or changes after start-up.
#[derive(Default)]
struct Widgets {
    values: [Handle<Label<Msg>>; 5],
    mode: Handle<ModeChoice>,
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
                        build(ModeChoice::new).bind(&self.mode),
                        row().gap(12).children((
                            column().gap(8).children(fields),
                            label("Used in Manual mode.").align(Align::Start),
                        )),
                        row().gap(8).children((
                            button("Apply").on_click(Msg::Apply).width(90),
                            button("Renew lease").on_click(Msg::Renew).width(120),
                            button("Revert").on_click(Msg::Revert).width(90),
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
        match stack::status() {
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

    /// Put the saved configuration (or the live address) into the form.
    fn fill(&self) {
        let stored = ["mode", "address", "gateway", "dns"].map(|name| {
            match self.store.get(&model::key(name)) {
                Some(Value::Str(text)) => Some(text),
                _ => None,
            }
        });
        let form = model::form_from(
            stored,
            self.status.as_ref().unwrap_or(&NetStatus::default()),
        );
        self.w.mode.get().group.select(usize::from(form.manual));
        self.w.address.get().set_text(&form.address);
        self.w.gateway.get().set_text(&form.gateway);
        self.w.dns.get().set_text(&form.dns);
    }

    fn form(&self) -> Form {
        Form {
            manual: self.w.mode.get().group.selected() == 1,
            address: self.w.address.get().text(),
            gateway: self.w.gateway.get().text(),
            dns: self.w.dns.get().text(),
        }
    }

    /// Check the form and write it to `confd`.
    fn apply(&self) {
        let form = self.form();
        let writes = match model::plan(&form) {
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
        println!("NETAPP:APPLY:PASS mode={mode}");
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
            reported: None,
            filled: false,
        };
        app.refresh();
        Ok(app)
    })
}
