//! The Wi-Fi join forms of the Network app: a WPA2 network that is not saved
//! yet asks for its password (the *known* form: the name is fixed);
//! "Other network..." asks for a name, the security and the password of a
//! network that may be hidden (the *other* form). They are two groups of
//! their own, one shown at a time: a form that hid and showed single rows
//! left some of them painted over the list.
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

/// One join form's widgets. The *known* form has no name or security rows
/// (their handles stay empty).
#[derive(Default)]
pub(super) struct JoinForm {
    group: Handle<GroupBox<Msg>>,
    title: Handle<Label<Msg>>,
    ssid: Handle<Edit<Msg>>,
    security: Handle<RadioGroup<Msg>>,
    secret: Handle<Edit<Msg>>,
    system: Handle<CheckBox<Msg>>,
    message: Handle<Label<Msg>>,
    cancel: Handle<Button<Msg>>,
    connect: Handle<Button<Msg>>,
}

/// Both forms.
#[derive(Default)]
pub(super) struct JoinWidgets {
    known: JoinForm,
    other: JoinForm,
}

impl JoinForm {
    fn layout(&self, other: bool) -> impl IntoEntry<Msg> {
        let mut fields = Vec::new();
        if other {
            fields.push(label("Network name").into_entry());
            fields.push(
                edit()
                    .placeholder("the name of the network")
                    .bind(&self.ssid)
                    .into_entry(),
            );
            fields.push(label("Security").into_entry());
            fields.push(
                radio_group(&["Open (no password)", "WPA2 (password)"])
                    .bind(&self.security)
                    .into_entry(),
            );
        }
        fields.push(label("Password").into_entry());
        fields.push(
            edit()
                .password()
                .placeholder("8 to 63 characters, or 64 hex digits")
                .bind(&self.secret)
                .into_entry(),
        );
        group(
            if other {
                "Join another Wi-Fi network"
            } else {
                "Join a Wi-Fi network"
            },
            column().gap(10).children((
                label("").bind(&self.title),
                grid([Track::Fixed(Dip(110.0)), Track::Fill(1)])
                    .gap(8)
                    .children(fields),
                checkbox("Available to all users (an administrator approves)").bind(&self.system),
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

    /// Report the controls' rectangles to `put` (UI probe images only).
    fn probe(&self, mut put: impl FnMut(&str, WidgetId)) {
        if let Some(ssid) = self.ssid.try_get() {
            put("join_ssid", ssid.id());
        }
        put("join_secret", self.secret.get().id());
        put("join_system", self.system.get().id());
        put("join_cancel", self.cancel.get().id());
        put("join_ok", self.connect.get().id());
        if let Some(security) = self.security.try_get() {
            for (name, index) in [("join_open", 0), ("join_wpa2", 1)] {
                if let Some(id) = security.ids().get(index) {
                    put(name, *id);
                }
            }
        }
    }

    /// Empty the password field (and the rest of the form).
    fn clear(&self) {
        self.secret.get().set_text("");
        if let Some(ssid) = self.ssid.try_get() {
            ssid.set_text("");
        }
        if let Some(security) = self.security.try_get() {
            security.select(1);
        }
        self.system.get().set_checked(false);
        self.message.get().set_text("");
    }
}

impl JoinWidgets {
    pub(super) fn known_layout(&self) -> impl IntoEntry<Msg> {
        self.known.layout(false)
    }

    pub(super) fn other_layout(&self) -> impl IntoEntry<Msg> {
        self.other.layout(true)
    }

    /// Show the form `kind` uses and hide the other (`None`: both). The
    /// other form's second radio button is a node of its own that hiding the
    /// group does not reach: it is hidden and shown with the group.
    pub(super) fn show(&self, ui: &Ui<Msg>, kind: Option<&JoinKind>) {
        ui.set_visible(
            self.known.group.get().id(),
            matches!(kind, Some(JoinKind::Known { .. })),
        );
        let other = matches!(kind, Some(JoinKind::Other));
        ui.set_visible(self.other.group.get().id(), other);
        if let Some(security) = self.other.security.try_get() {
            for id in security.ids() {
                ui.set_visible(id, other);
            }
        }
    }

    /// The form `kind` uses.
    fn form(&self, kind: &JoinKind) -> &JoinForm {
        match kind {
            JoinKind::Known { .. } => &self.known,
            JoinKind::Other => &self.other,
        }
    }

    /// The probe lines of the form `kind` uses.
    pub(super) fn probe(&self, kind: &JoinKind, put: impl FnMut(&str, WidgetId)) {
        self.form(kind).probe(put);
    }

    /// Empty both forms.
    pub(super) fn clear(&self) {
        self.known.clear();
        self.other.clear();
    }
}

impl Network {
    /// Replace the list with the form `kind` uses.
    pub(super) fn open_join(&mut self, kind: JoinKind) {
        let forms = &self.w.wifi.join;
        forms.clear();
        let form = forms.form(&kind);
        let (title, other) = match &kind {
            JoinKind::Known { ssid } => (
                format!("\"{ssid}\" is protected. Enter its password."),
                false,
            ),
            JoinKind::Other => (
                String::from("Join a network that is not in the list, or is hidden."),
                true,
            ),
        };
        form.title.get().set_text(&title);
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

    /// Say `text` on the form in use.
    pub(super) fn join_say(&self, text: &str) {
        if let Some(kind) = &self.wifi.join {
            self.w.wifi.join.form(kind).message.get().set_text(text);
        }
    }

    /// Connect: check the form, save the network and start joining it.
    pub(super) fn join_submit(&mut self) {
        let Some(kind) = self.wifi.join.clone() else {
            return;
        };
        let form = self.w.wifi.join.form(&kind);
        let typed = form.ssid.try_get().map(|e| e.text()).unwrap_or_default();
        let wpa2 = form
            .security
            .try_get()
            .is_none_or(|radio| radio.selected() == 1);
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
    wifi::add_network(&join.ssid, join.security, join.hidden, passphrase).map_err(|error| {
        let slow = if error == wifi::Error::Code(-110) {
            " It may still be saved: look for it in the list in a moment."
        } else {
            ""
        };
        format!("Could not save the network: {}.{slow}", error.describe())
    })
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
