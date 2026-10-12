//! The Wi-Fi join form of the Network app: a WPA2 network that is not saved
//! yet asks for its password; "Other network..." asks for a name, the
//! security and the password of a network that may be hidden.
//!
//! **The password** is read from the masked field once, when Connect is
//! pressed, checked (`wifiview::check_join`) and passed by value to
//! `wifi::add_network`, which wipes it after the call (or to `elevd`'s
//! `net.wifi.add` for a network shared with all users, where the arguments
//! are wiped here). The field is cleared before the call returns control to
//! the window, whatever the outcome; nothing here keeps the text, logs it or
//! prints it, and the masked field refuses copy and cut.
//!
//! "Available to all users" saves the network for everyone, before login
//! too: that is the machine's setting, so an administrator approves it on
//! the trusted prompt (`elevd`, action `net.wifi.add`, which stores the
//! password in `keyd` and the profile in `wlanmd` in one step).

use xui_app::net::wifi::{self, Security};
use xui_app::net::wifiview::{self, Join, JoinKind};
use xui_app::platform::elevd;
use xui_core::app::Ui;
use xui_core::backend::WidgetId;
use xui_core::prelude::*;

use super::wifi_page::View;
use super::{Msg, Network};

/// The join form's widgets.
#[derive(Default)]
pub(super) struct JoinWidgets {
    group: Handle<GroupBox<Msg>>,
    title: Handle<Label<Msg>>,
    ssid_caption: Handle<Label<Msg>>,
    ssid: Handle<Edit<Msg>>,
    security_caption: Handle<Label<Msg>>,
    security: Handle<RadioGroup<Msg>>,
    secret: Handle<Edit<Msg>>,
    system: Handle<CheckBox<Msg>>,
    message: Handle<Label<Msg>>,
    cancel: Handle<Button<Msg>>,
    connect: Handle<Button<Msg>>,
}

impl JoinWidgets {
    pub(super) fn layout(&self) -> impl IntoEntry<Msg> {
        let fields = vec![
            label("Network name").bind(&self.ssid_caption).into_entry(),
            edit()
                .placeholder("the name of the network")
                .bind(&self.ssid)
                .into_entry(),
            label("Security").bind(&self.security_caption).into_entry(),
            radio_group(&["Open (no password)", "WPA2 (password)"])
                .bind(&self.security)
                .into_entry(),
            label("Password").into_entry(),
            edit()
                .password()
                .placeholder("8 to 63 characters, or 64 hex digits")
                .bind(&self.secret)
                .into_entry(),
        ];
        group(
            "Join a Wi-Fi network",
            column().gap(10).children((
                label("").bind(&self.title),
                grid([Track::Fixed(Dip(110.0)), Track::Fill(1)])
                    .gap(8)
                    .children(fields),
                checkbox("Available to all users (an administrator approves)")
                    .bind(&self.system),
                label("").bind(&self.message).fixed(44),
                row().gap(8).children((
                    button("Cancel")
                        .on_click(Msg::JoinCancel)
                        .bind(&self.cancel)
                        .width(90),
                    button("Connect")
                        .on_click(Msg::JoinOk)
                        .bind(&self.connect)
                        .primary()
                        .width(90),
                )),
            )),
        )
        .bind(&self.group)
        .fill(1)
    }

    pub(super) fn group_id(&self) -> WidgetId {
        self.group.get().id()
    }

    /// Report the controls' rectangles to `put` (UI probe images only).
    pub(super) fn probe(&self, _ui: &Ui<Msg>, mut put: impl FnMut(&str, WidgetId)) {
        put("join_ssid", self.ssid.get().id());
        put("join_secret", self.secret.get().id());
        put("join_system", self.system.get().id());
        put("join_cancel", self.cancel.get().id());
        put("join_ok", self.connect.get().id());
        if let Some(id) = self.security.get().ids().get(1) {
            put("join_wpa2", *id);
        }
    }

    /// Empty the password field (and the rest of the form).
    fn clear(&self) {
        self.secret.get().set_text("");
        self.ssid.get().set_text("");
        self.system.get().set_checked(false);
        self.message.get().set_text("");
    }

    /// Show or hide the rows only "Other network" has.
    fn other_rows(&self, ui: &Ui<Msg>, shown: bool) {
        ui.set_visible(self.ssid_caption.get().id(), shown);
        ui.set_visible(self.ssid.get().id(), shown);
        ui.set_visible(self.security_caption.get().id(), shown);
        for id in self.security.get().ids() {
            ui.set_visible(id, shown);
        }
    }
}

impl Network {
    /// Replace the list with the join form.
    pub(super) fn open_join(&mut self, kind: JoinKind) {
        let form = &self.w.wifi.join;
        form.clear();
        form.security.get().select(1);
        let (title, other) = match &kind {
            JoinKind::Known { ssid } => (format!("\"{ssid}\" is protected. Enter its password."), false),
            JoinKind::Other => (
                String::from("Join a network that is not in the list, or is hidden."),
                true,
            ),
        };
        form.title.get().set_text(&title);
        form.other_rows(&self.ui, other);
        println!(
            "NETAPP:WIFI:JOIN:OPEN {}",
            if other { "other" } else { "known" }
        );
        if other {
            form.ssid.get().focus();
        } else {
            form.secret.get().focus();
        }
        self.wifi.join = Some(kind);
        self.wifi_show(View::Join);
    }

    pub(super) fn wifi_other(&mut self) {
        self.open_join(JoinKind::Other);
    }

    pub(super) fn join_cancel(&mut self) {
        self.w.wifi.join.clear();
        self.wifi.join = None;
        self.wifi_show(View::List);
    }

    pub(super) fn join_say(&self, text: &str) {
        self.w.wifi.join.message.get().set_text(text);
    }

    /// Connect: check the form, save the network and start joining it.
    pub(super) fn join_submit(&mut self) {
        let Some(kind) = self.wifi.join.clone() else {
            return;
        };
        let form = &self.w.wifi.join;
        let typed = form.ssid.get().text();
        let wpa2 = form.security.get().selected() == 1;
        let shared = form.system.get().is_checked();
        let secret = form.secret.get().text();
        let join = match wifiview::check_join(&kind, &typed, wpa2, &secret) {
            Ok(join) => join,
            Err(why) => {
                wifi::wipe_string(secret);
                self.join_say(why);
                return;
            }
        };
        // Whatever happens next, the field no longer holds the password.
        form.secret.get().set_text("");
        let saved = if shared {
            join_shared(&join, secret)
        } else {
            join_own(&join, secret)
        };
        match saved {
            Ok(id) => {
                self.w.wifi.join.clear();
                self.wifi.join = None;
                self.wifi_show(View::List);
                self.wifi_join_saved(&join.ssid, &id);
            }
            Err(why) => self.join_say(&why),
        }
    }
}

/// Save `join` for the calling user; the password is wiped by the client.
fn join_own(join: &Join, secret: String) -> std::result::Result<String, String> {
    let passphrase = match join.security {
        Security::Wpa2Psk => Some(secret),
        _ => {
            wifi::wipe_string(secret);
            None
        }
    };
    wifi::add_network(&join.ssid, join.security, join.hidden, passphrase)
        .map_err(|error| format!("Could not save the network: {}", error.describe()))
}

/// Save `join` for all users through `elevd`: it stores the password and the
/// profile after an administrator approved, and answers with the id.
fn join_shared(join: &Join, secret: String) -> std::result::Result<String, String> {
    let args = wifiview::elevd_add_args(join, &secret);
    wifi::wipe_string(secret);
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let answer = elevd::request("net.wifi.add", &refs);
    for arg in args {
        wifi::wipe_string(arg);
    }
    match answer {
        Ok((_, values)) => values
            .into_iter()
            .next()
            .ok_or_else(|| String::from("The network was saved but its id did not come back.")),
        Err(error) => Err(format!("Not saved: {}", elevd::describe(&error))),
    }
}
