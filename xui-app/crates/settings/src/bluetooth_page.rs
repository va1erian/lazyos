//! The Bluetooth page (docs/bluetooth-plan.md B5): the devices `btd` knows,
//! a scan, pairing with the passkey or the number shown, forgetting a device.
//!
//! The page never waits on `btd`: it asks through [`Bluetooth`], whose calls
//! only queue a request, and applies the [`BtEvent`]s that come back as
//! messages ([`BtMsg::Event`]). The pairing dialog is part of the page, not a
//! window of its own: it shows the six digits to type on a keyboard, or the
//! six digits to compare with Accept and Reject, until the pairing ends.

use std::rc::Rc;
use std::sync::{Arc, Mutex};

use xui_core::app::{Proxy, Ui};
use xui_core::arrange::{button, column, label, panel, row, Build, Handle, LayoutExt, Mounted};
use xui_core::backend::{Result, WidgetId};
use xui_core::layout::Align;
use xui_core::widget::{Button, Label, ListView, Panel};
use xui_core::HasText;

use crate::app::{choice_list, Msg};
use crate::bluetooth::{Bluetooth, BtEvent, BtModel, BtPairing, BtPhase, BtSink};

/// Messages the Bluetooth page raises.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BtMsg {
    /// A device row was selected.
    Select(usize),
    Scan,
    Pair,
    Forget,
    /// Accept the number shown.
    Accept,
    /// Reject the number shown.
    Reject,
    /// Cancel the pairing in progress.
    Cancel,
    /// The service said something (from a worker thread).
    Event(BtEvent),
}

/// A button raising `msg`.
fn command(text: &str, msg: BtMsg) -> Build<Button<Msg>, Msg> {
    button(text).on_click(Msg::Bt(msg))
}

/// The page's widgets and what it shows.
pub struct BluetoothPage {
    list: Rc<ListView<Msg>>,
    header: Rc<Label<Msg>>,
    scan: Rc<Button<Msg>>,
    pair: Rc<Button<Msg>>,
    forget: Rc<Button<Msg>>,
    dialog: Rc<Panel<Msg>>,
    headline: Rc<Label<Msg>>,
    detail: Rc<Label<Msg>>,
    accept: Rc<Button<Msg>>,
    reject: Rc<Button<Msg>>,
    cancel: Rc<Button<Msg>>,
    _mounted: Mounted<Msg>,
    model: BtModel,
    /// `start` was called (the first time the page was shown).
    started: bool,
}

/// Six digits, as a keyboard asks for them.
pub fn digits(n: u32) -> String {
    format!("{:06}", n % 1_000_000)
}

impl BluetoothPage {
    /// Lays the page out in the container `page`.
    pub fn build(ui: &Ui<Msg>, page: WidgetId) -> Result<BluetoothPage> {
        let (list, header, headline, detail) =
            (Handle::new(), Handle::new(), Handle::new(), Handle::new());
        let (scan, pair, forget) = (Handle::new(), Handle::new(), Handle::new());
        let (accept, reject, cancel) = (Handle::new(), Handle::new(), Handle::new());
        let dialog = Handle::new();
        let mounted = ui.mount_in(
            page,
            column().padding(12).gap(6).children((
                label("").bind(&header),
                choice_list(&[])
                    .on_select(|i| Msg::Bt(BtMsg::Select(i)))
                    .bind(&list)
                    .size(440, 140)
                    .align(Align::Start),
                row().gap(6).children((
                    command("Scan", BtMsg::Scan).bind(&scan),
                    command("Pair", BtMsg::Pair).bind(&pair),
                    command("Forget", BtMsg::Forget).bind(&forget),
                )),
                panel(column().padding(10).gap(6).children((
                    label("").title().bind(&headline),
                    label("").bind(&detail),
                    row().gap(6).children((
                        command("Accept", BtMsg::Accept).bind(&accept),
                        command("Reject", BtMsg::Reject).bind(&reject),
                        command("Cancel", BtMsg::Cancel).bind(&cancel),
                    )),
                )))
                .bind(&dialog)
                .size(440, 110)
                .align(Align::Start),
            )),
        )?;
        let page = BluetoothPage {
            list: list.get(),
            header: header.get(),
            scan: scan.get(),
            pair: pair.get(),
            forget: forget.get(),
            dialog: dialog.get(),
            headline: headline.get(),
            detail: detail.get(),
            accept: accept.get(),
            reject: reject.get(),
            cancel: cancel.get(),
            _mounted: mounted,
            model: BtModel::default(),
            started: false,
        };
        ui.set_visible(page.dialog.id(), false);
        page.render(ui);
        Ok(page)
    }

    /// The page was shown: connect to the service the first time, and ask
    /// for a fresh picture every time.
    pub fn show(&mut self, bluetooth: &dyn Bluetooth, proxy: Proxy<Msg>) {
        if !self.started {
            self.started = true;
            // The proxy is `Send`; the sink is shared by two worker threads.
            let proxy = Mutex::new(proxy);
            let sink: BtSink = Arc::new(move |event| {
                if let Ok(proxy) = proxy.lock() {
                    let _ = proxy.send(Msg::Bt(BtMsg::Event(event)));
                }
            });
            bluetooth.start(sink);
        } else {
            bluetooth.refresh();
        }
    }

    /// Handle one message; returns the status line text (empty: none).
    pub fn update(&mut self, ui: &Ui<Msg>, msg: BtMsg, bluetooth: &dyn Bluetooth) -> String {
        let mut text = String::new();
        match msg {
            BtMsg::Select(row) => {
                self.list
                    .select(Some(row).filter(|row| *row < self.model.devices.len()));
            }
            BtMsg::Scan => {
                if self.model.discovering {
                    bluetooth.stop_scan();
                } else {
                    bluetooth.scan();
                    // The list says so at once; the service confirms.
                    self.model.discovering = true;
                }
            }
            BtMsg::Pair => match self.selected_address() {
                Some(address) => bluetooth.pair(&address),
                None => text = "Select a device first.".into(),
            },
            BtMsg::Forget => match self.selected_address() {
                Some(address) => bluetooth.forget(&address),
                None => text = "Select a device first.".into(),
            },
            BtMsg::Accept | BtMsg::Reject => {
                let accept = msg == BtMsg::Accept;
                bluetooth.confirm(&self.model.pairing.address, accept);
            }
            BtMsg::Cancel => bluetooth.cancel(),
            BtMsg::Event(event) => {
                if let BtEvent::Failed(why) = &event {
                    text = why.clone();
                }
                let before = self.selected_address();
                self.model.apply(event);
                self.restore_selection(before);
            }
        }
        self.render(ui);
        text
    }

    fn selected_address(&self) -> Option<String> {
        let row = self.list.selected()?;
        self.model.at(row).map(|d| d.address.clone())
    }

    /// Keep the same device selected when the list changes under it.
    fn restore_selection(&mut self, address: Option<String>) {
        let rows: Vec<String> = self.model.devices.iter().map(|d| d.row()).collect();
        let rows: Vec<&str> = rows.iter().map(String::as_str).collect();
        self.list.set_items(&rows);
        let again = address
            .and_then(|a| self.model.devices.iter().position(|d| d.address == a))
            .or(if rows.is_empty() { None } else { Some(0) });
        self.list.select(again);
    }

    /// Show the model: the header, the buttons, the pairing dialog.
    fn render(&self, ui: &Ui<Msg>) {
        let m = &self.model;
        let header = match (&m.problem, m.controller, m.discovering) {
            (Some(problem), _, _) => problem.clone(),
            (None, false, _) => "No Bluetooth controller was found.".into(),
            (None, true, true) => "Scanning for keyboards and mice...".into(),
            (None, true, false) if m.devices.is_empty() => {
                "No devices yet. Put a keyboard or mouse in pairing mode and press Scan.".into()
            }
            (None, true, false) => "Bluetooth devices".into(),
        };
        self.header.set_text(&header);
        let pairing = m.pairing.active();
        let selected = self.list.selected().and_then(|row| m.at(row));
        self.scan.set_text(if m.discovering {
            "Stop scanning"
        } else {
            "Scan"
        });
        self.scan.set_enabled(m.controller && !pairing);
        self.pair
            .set_enabled(m.controller && !pairing && selected.is_some_and(|d| !d.bonded));
        self.forget.set_enabled(selected.is_some() && !pairing);
        self.render_dialog(ui);
    }

    fn render_dialog(&self, ui: &Ui<Msg>) {
        let p = &self.model.pairing;
        let name = self
            .model
            .pairing_device()
            .map(|d| {
                if d.name.is_empty() {
                    d.address.clone()
                } else {
                    d.name.clone()
                }
            })
            .unwrap_or_else(|| p.address.clone());
        let (headline, detail) = dialog_text(p, &name);
        let shown = !matches!(p.phase, BtPhase::Idle);
        ui.set_visible(self.dialog.id(), shown);
        self.headline.set_text(&headline);
        self.detail.set_text(&detail);
        let number = p.phase == BtPhase::ConfirmNumber;
        for button in [&self.accept, &self.reject] {
            ui.set_visible(button.id(), number);
        }
        ui.set_visible(self.cancel.id(), p.active() && !number);
    }
}

/// The dialog's two lines for a pairing state with `name`.
fn dialog_text(p: &BtPairing, name: &str) -> (String, String) {
    match p.phase {
        BtPhase::Idle => (String::new(), String::new()),
        BtPhase::Connecting => (
            format!("Pairing with {name}..."),
            "Keep the device in pairing mode, close to the computer.".into(),
        ),
        BtPhase::ShowPasskey => (
            format!("Type {} on {name}", digits(p.passkey)),
            "Then press Enter on that keyboard.".into(),
        ),
        BtPhase::ConfirmNumber => (
            format!("Does {name} show {}?", digits(p.number)),
            "Accept only if both screens show the same six digits.".into(),
        ),
        BtPhase::Paired => (
            format!("{name} is paired."),
            "It will connect by itself from now on, also at the login screen.".into(),
        ),
        BtPhase::Failed => (format!("Could not pair {name}."), p.reason.text().into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bluetooth::BtReason;

    fn pairing(phase: BtPhase) -> BtPairing {
        BtPairing {
            phase,
            address: "AA:BB:CC:DD:EE:01".into(),
            passkey: 4217,
            number: 987_654,
            reason: BtReason::Rejected,
        }
    }

    #[test]
    fn digits_are_six_with_leading_zeros() {
        assert_eq!(digits(4217), "004217");
        assert_eq!(digits(0), "000000");
        assert_eq!(digits(999_999), "999999");
        assert_eq!(digits(1_234_567), "234567");
    }

    #[test]
    fn the_dialog_tells_the_user_what_to_do() {
        let (h, d) = dialog_text(&pairing(BtPhase::ShowPasskey), "Desk");
        assert_eq!(h, "Type 004217 on Desk");
        assert!(d.contains("Enter"));
        let (h, _) = dialog_text(&pairing(BtPhase::ConfirmNumber), "Desk");
        assert_eq!(h, "Does Desk show 987654?");
        let (h, d) = dialog_text(&pairing(BtPhase::Failed), "Desk");
        assert_eq!(h, "Could not pair Desk.");
        assert!(d.contains("rejected"));
        assert_eq!(
            dialog_text(&pairing(BtPhase::Idle), "x"),
            (String::new(), String::new())
        );
    }
}
