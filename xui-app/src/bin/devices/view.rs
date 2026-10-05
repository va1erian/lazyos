//! The `devices` window's widgets: the layout, and how each refresh fills it.
//!
//! Three group boxes stacked in one column rather than three tabs: the rule
//! and refusal sections are usually a handful of rows, so at 900x640 all three
//! fit at once and the window answers "who holds what, and what is refused"
//! without a click (the screenshot run also captures it without switching
//! tabs). The device table takes twice the leftover height of the others; each
//! list scrolls when it overflows instead of eliding rows.

use devinspect::{class_name, reason_name, Denial, Device, Uid};
use xui_app::devinfo::{grants, DevView};
use xui_app::format::uptime;
use xui_core::prelude::*;

use crate::Msg;

/// The keys the header reminds of.
const KEYS: &str = "device syscall 23 · read-only · 2 s refresh · [r] refresh  [q] quit";

/// Every widget the app changes after start-up.
#[derive(Default)]
pub(crate) struct Widgets {
    devices_heading: Handle<Label<Msg>>,
    devices: Handle<ListView<Msg>>,
    rules_heading: Handle<Label<Msg>>,
    rules_note: Handle<Label<Msg>>,
    rules: Handle<ListView<Msg>>,
    denials_heading: Handle<Label<Msg>>,
    denials: Handle<ListView<Msg>>,
    status: Handle<StatusBar<Msg>>,
}

impl Widgets {
    /// The whole window: a header, the three sections and the status bar.
    pub(crate) fn layout(&self) -> Layout<Msg> {
        column().padding(12).gap(10).children((
            row()
                .gap(16)
                .align(Align::Center)
                .children((label("Devices").title(), label(KEYS))),
            group("Devices", self.devices_section()).fill(2),
            group("Driver class rules", self.rules_section()).fill(1),
            group("Refused claims", self.denials_section()).fill(1),
            status_bar(&["", "", "devctl shows the same from a shell"]).bind(&self.status),
        ))
    }

    fn devices_section(&self) -> Layout<Msg> {
        column().padding(8).gap(6).children((
            label("").bind(&self.devices_heading),
            list()
                .column_right("id", 44)
                .column("class", 116)
                .column("PCI", 100)
                .column("vendor:dev", 110)
                .column("owner", 130)
                .column("rights", Fill)
                .bind(&self.devices)
                .fill(1),
        ))
    }

    fn rules_section(&self) -> Layout<Msg> {
        column().padding(8).gap(6).children((
            label("").bind(&self.rules_heading),
            list()
                .column("driver", 160)
                .column("class", 120)
                .column("may", 220)
                .column("verdict", Fill)
                .bind(&self.rules)
                .fill(1),
            label("").bind(&self.rules_note),
        ))
    }

    fn denials_section(&self) -> Layout<Msg> {
        column().padding(8).gap(6).children((
            label("").bind(&self.denials_heading),
            list()
                .column("time", 110)
                .column("uid", 160)
                .column("class", 120)
                .column_right("device", 80)
                .column("why", Fill)
                .bind(&self.denials)
                .fill(1),
        ))
    }

    /// Fills every section and the status bar from `view`.
    pub(crate) fn show(&self, view: &DevView, refreshes: u64) {
        let drivers = match &view.devices {
            Ok(devices) => {
                self.show_devices(devices);
                devices
                    .iter()
                    .filter(|device| device.owner.is_some_and(|uid| uid != 0))
                    .count()
                    .to_string()
            }
            Err(code) => {
                self.show_unavailable(*code);
                String::from("?")
            }
        };
        self.show_rules(view);
        self.show_denials(&view.denials);
        let status = self.status.get();
        status.set_text(0, &format!("{refreshes} refresh(es)"));
        status.set_text(
            1,
            &format!("{drivers} device(s) held by unprivileged drivers"),
        );
    }

    fn show_devices(&self, devices: &[Device]) {
        let claimed = devices
            .iter()
            .filter(|device| device.owner.is_some())
            .count();
        self.devices_heading
            .get()
            .set_text(&format!("{} found, {claimed} claimed", devices.len()));
        let rows: Vec<Vec<String>> = devices
            .iter()
            .map(|device| {
                vec![
                    device.id.to_string(),
                    device.class_name().to_string(),
                    format!(
                        "{:02x}/{:02x}/{:02x}",
                        device.class, device.subclass, device.prog_if
                    ),
                    format!("{:04x}:{:04x}", device.vendor, device.device),
                    device
                        .owner
                        .map_or(String::from("free"), |uid| Uid(uid).to_string()),
                    device.rights.to_string(),
                ]
            })
            .collect();
        self.devices.get().refresh_model(rows);
    }

    /// Says why the inventory cannot be read, where the device heading goes.
    fn show_unavailable(&self, code: i64) {
        let why = if code == devinspect::errno::EACCES {
            String::from("the Messenger policy refuses os.kernel.dev to this app")
        } else {
            format!("the device syscall returned errno {code}")
        };
        self.devices_heading
            .get()
            .set_text(&format!("Device inventory unavailable: {why}"));
        self.devices.get().refresh_model(Vec::<Vec<String>>::new());
    }

    fn show_rules(&self, view: &DevView) {
        let (heading, note) = match &view.rules {
            Ok(Some(rules)) => (
                format!("{} enforced since boot", rules.len()),
                "every other non-root uid is refused every device class; root keeps its authority",
            ),
            Ok(None) => (
                String::from("none installed"),
                "without class rules, claims are judged by the Messenger policy alone",
            ),
            Err(code) => (
                format!("unreadable (errno {code})"),
                "without class rules, claims are judged by the Messenger policy alone",
            ),
        };
        self.rules_heading.get().set_text(&heading);
        self.rules_note.get().set_text(note);
        let rows: Vec<Vec<String>> = match &view.rules {
            Ok(Some(rules)) => grants(rules)
                .into_iter()
                .map(|grant| {
                    vec![
                        Uid(grant.uid).to_string(),
                        class_name(grant.class_id).to_string(),
                        grant.methods.join(" "),
                        String::from(if grant.allow { "allow" } else { "deny" }),
                    ]
                })
                .collect(),
            _ => Vec::new(),
        };
        self.rules.get().refresh_model(rows);
    }

    fn show_denials(&self, denials: &std::result::Result<Vec<Denial>, i64>) {
        let heading = match denials {
            Ok(denials) if denials.is_empty() => {
                String::from("none: every claim so far was granted")
            }
            Ok(denials) => format!("{} in the audit ring", denials.len()),
            Err(code) if *code == devinspect::errno::EPERM => String::from(
                "reading the audit ring needs CAP_AUDIT_READ; desktop sessions run without capabilities",
            ),
            Err(code) => format!("unreadable (errno {code})"),
        };
        self.denials_heading.get().set_text(&heading);
        let rows: Vec<Vec<String>> = denials
            .iter()
            .flatten()
            .map(|denial| {
                vec![
                    uptime(denial.ticks),
                    Uid(denial.uid).to_string(),
                    class_name(denial.class_id).to_string(),
                    denial.device.to_string(),
                    reason_name(denial.reason).to_string(),
                ]
            })
            .collect();
        self.denials.get().refresh_model(rows);
    }
}
