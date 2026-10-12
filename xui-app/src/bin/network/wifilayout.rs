//! The Wi-Fi page's widgets and layout (see `wifipage.rs`).

use xui_core::prelude::*;
use xui_core::widget::CheckBox;

use crate::Msg;

/// What the page's buttons ask for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Scan,
    Join,
    JoinHidden,
    Leave,
    Forget,
}

/// Every widget the page reads or changes after start-up.
#[derive(Default)]
pub struct Widgets {
    pub(super) heading: Handle<Label<Msg>>,
    pub(super) list: Handle<ListView<Msg>>,
    pub(super) passphrase: Handle<Edit<Msg>>,
    pub(super) hidden: Handle<Edit<Msg>>,
    pub(super) open: Handle<CheckBox<Msg>>,
    pub(super) scan: Handle<Button<Msg>>,
    pub(super) join: Handle<Button<Msg>>,
    pub(super) leave: Handle<Button<Msg>>,
    pub(super) forget: Handle<Button<Msg>>,
    pub(super) join_hidden: Handle<Button<Msg>>,
    pub(super) message: Handle<Label<Msg>>,
}

impl Widgets {
    pub(super) fn layout(&self) -> Layout<Msg> {
        column()
            .padding(Insets::new(Dip(12.0), Dip(8.0), Dip(12.0), Dip(8.0)))
            .gap(8)
            .children((
                label("Looking for a Wi-Fi radio...")
                    .bind(&self.heading)
                    .fixed(24),
                list()
                    .column("Network", 220)
                    .column("Signal", 130)
                    .column("Security", 90)
                    .column("State", Fill)
                    .on_activate(|_| Msg::Wifi(Action::Join))
                    .bind(&self.list)
                    .fill(1),
                row().gap(8).children((
                    label("Passphrase").width(80).align(Align::Center),
                    edit()
                        .password()
                        .placeholder("for a new WPA2 network")
                        .bind(&self.passphrase)
                        .width(220),
                    button("Join")
                        .on_click(Msg::Wifi(Action::Join))
                        .bind(&self.join)
                        .width(80),
                    button("Disconnect")
                        .on_click(Msg::Wifi(Action::Leave))
                        .bind(&self.leave)
                        .width(100),
                )),
                row().gap(8).children((
                    button("Scan")
                        .on_click(Msg::Wifi(Action::Scan))
                        .bind(&self.scan)
                        .width(80),
                    button("Forget")
                        .on_click(Msg::Wifi(Action::Forget))
                        .bind(&self.forget)
                        .width(80),
                )),
                group(
                    "Hidden network",
                    row().gap(8).children((
                        label("Name").width(80).align(Align::Center),
                        edit()
                            .placeholder("the network's name")
                            .bind(&self.hidden)
                            .width(220),
                        button("Join hidden")
                            .on_click(Msg::Wifi(Action::JoinHidden))
                            .bind(&self.join_hidden)
                            .width(110),
                        checkbox("Open (no passphrase)").bind(&self.open),
                    )),
                ),
                label("").bind(&self.message).fixed(44),
            ))
    }
}
