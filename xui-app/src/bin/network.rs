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

use std::rc::Rc;

use xui_app::backend::LazyOSBackend;
use xui_app::net::model::{self, Form, NetStatus, Write};
use xui_app::net::stack;
use xui_app::platform::confd_store::ConfdStore;
use xui_confd_editor::store::StoreError as ConfStoreError;
use xui_core::app::{run_app, App, Ui};
use xui_core::backend::{Backend, PlatformSpec};
use xui_core::widget::{Button, Edit, GroupBox, Label, RadioGroup};
use xui_core::{Dip, HasText, Rect};
use xui_settings::store::{ConfigStore, Value};

/// The window size when a compositor lays the app out.
const WINDOW: (i32, i32) = (600, 470);
/// How often the status refreshes.
const REFRESH_MILLIS: u32 = 1000;

enum Msg {
    Tick,
    Apply,
    Renew,
    Revert,
    Close,
}

fn rect(x: i32, y: i32, w: i32, h: i32) -> Rect {
    Rect::new(x, y, x + w, y + h)
}

/// The status rows: caption, then the line it shows.
const STATUS_ROWS: [&str; 5] = ["Interface", "Mode", "Address", "Gateway", "Traffic"];

struct Network {
    values: Vec<Label<Msg>>,
    mode: RadioGroup<Msg>,
    address: Edit<Msg>,
    gateway: Edit<Msg>,
    dns: Edit<Msg>,
    message: Label<Msg>,
    store: ConfdStore,
    status: Option<NetStatus>,
    /// The address last reported on serial, so a change is reported once.
    reported: Option<String>,
    /// Whether the form has been filled (it waits for the first status, so
    /// Manual starts from the live address).
    filled: bool,
    _keep: (Vec<Label<Msg>>, Vec<Button<Msg>>, Vec<GroupBox<Msg>>),
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
                self.message
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
    fn build(ui: &Ui<Msg>) -> xui_core::backend::Result<Network> {
        let width = ui.client_rect().width().max(WINDOW.0);
        let inner = width - 24;
        let groups = vec![
            GroupBox::new(ui, rect(12, 8, inner, 156), "Status")?,
            GroupBox::new(ui, rect(12, 172, inner, 236), "Configuration")?,
        ];
        let mut labels = Vec::new();
        let mut values = Vec::new();
        for (row, caption) in STATUS_ROWS.iter().enumerate() {
            let y = 34 + row as i32 * 24;
            labels.push(Label::new(ui, rect(28, y, 90, 20), caption)?);
            values.push(Label::new(ui, rect(120, y, inner - 120, 20), "...")?);
        }
        let mode = RadioGroup::new(
            ui,
            rect(28, 196, 320, 56),
            &["Automatic (DHCP)", "Manual (static address)"],
        )?;
        let fields = [
            ("Address / prefix", 264),
            ("Gateway", 296),
            ("DNS server", 328),
        ];
        for (caption, y) in fields {
            labels.push(Label::new(ui, rect(28, y + 4, 130, 20), caption)?);
        }
        let address = Edit::new(ui, rect(160, 264, 200, 26), "")?.cue("192.168.1.20/24");
        let gateway = Edit::new(ui, rect(160, 296, 200, 26), "")?.cue("192.168.1.1 (optional)");
        let dns = Edit::new(ui, rect(160, 328, 200, 26), "")?.cue("10.0.2.3 (optional)");
        labels.push(Label::new(
            ui,
            rect(372, 268, inner - 372, 60),
            "Used in Manual mode.",
        )?);
        let buttons = vec![
            Button::new(ui, rect(28, 366, 90, 30), "Apply")?.on_click(|| Some(Msg::Apply)),
            Button::new(ui, rect(126, 366, 120, 30), "Renew lease")?.on_click(|| Some(Msg::Renew)),
            Button::new(ui, rect(254, 366, 90, 30), "Revert")?.on_click(|| Some(Msg::Revert)),
        ];
        let message = Label::new(ui, rect(16, 416, inner, 44), "")?;
        Ok(Network {
            values,
            mode,
            address,
            gateway,
            dns,
            message,
            store: ConfdStore::new(),
            status: None,
            reported: None,
            filled: false,
            _keep: (labels, buttons, groups),
        })
    }

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
                for (label, line) in self.values.iter().zip(lines) {
                    label.set_text(&line);
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
                self.values[0].set_text(&error.describe());
                for label in &self.values[1..] {
                    label.set_text("-");
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
        self.mode.select(usize::from(form.manual));
        self.address.set_text(&form.address);
        self.gateway.set_text(&form.gateway);
        self.dns.set_text(&form.dns);
    }

    fn form(&self) -> Form {
        Form {
            manual: self.mode.selected() == 1,
            address: self.address.text(),
            gateway: self.gateway.text(),
            dns: self.dns.text(),
        }
    }

    /// Check the form and write it to `confd`.
    fn apply(&self) {
        let form = self.form();
        let writes = match model::plan(&form) {
            Ok(writes) => writes,
            Err(why) => {
                println!("NETAPP:APPLY:REFUSED");
                self.message.set_text(&why);
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
                self.message.set_text(&format!("Could not save: {error}"));
                return;
            }
        }
        let mode = if form.manual { "static" } else { "dhcp" };
        println!("NETAPP:APPLY:PASS mode={mode}");
        self.message.set_text(if form.manual {
            "Saved. The network stack restarts with the manual address within a few seconds."
        } else {
            "Saved. The network stack restarts with DHCP within a few seconds."
        });
    }

    fn renew(&self) {
        match stack::renew() {
            Ok(()) => {
                println!("NETAPP:RENEW:PASS");
                self.message
                    .set_text("Asked the DHCP server for a fresh lease.");
            }
            Err(error) => {
                println!("NETAPP:RENEW:FAIL");
                self.message
                    .set_text(&format!("Renew failed: {}", error.describe()));
            }
        }
    }
}

fn main() -> std::process::ExitCode {
    let backend = match LazyOSBackend::connect() {
        Ok(backend) => Rc::new(backend),
        Err(code) => {
            println!("NETAPP:BIND:FAIL:{code}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let (width, height) = backend.window_size(WINDOW);
    backend.on_first_frame(|| println!("NETAPP:UP:PASS"));
    let spec = PlatformSpec::new("Network").size(Dip(width as f32), Dip(height as f32));
    let outcome = run_app(Rc::clone(&backend) as Rc<dyn Backend>, spec, |ui| {
        let mut app = match Network::build(ui) {
            Ok(app) => app,
            Err(error) => {
                println!("NETAPP:BUILD:FAIL:{error}");
                std::process::exit(1);
            }
        };
        app.refresh();
        ui.on_timer(|_| Some(Msg::Tick));
        ui.set_timer(REFRESH_MILLIS);
        ui.on_close(|| Some(Msg::Close));
        app
    });
    backend.unbind();
    match outcome {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            println!("NETAPP:RUN:FAIL:{error}");
            std::process::ExitCode::FAILURE
        }
    }
}
