//! The Bluetooth page's hook into the window: one of its messages arrived.
//! It does not wait on `btd`: the page queues requests on
//! [`Bluetooth`](crate::bluetooth::Bluetooth) and the answers return as
//! [`BtMsg::Event`](crate::bluetooth_page::BtMsg::Event) messages.

use xui_core::app::Ui;

use super::{say, Msg, SettingsApp};
use crate::bluetooth_page::BtMsg;

impl SettingsApp {
    pub(super) fn bluetooth_update(&mut self, ui: &Ui<Msg>, msg: BtMsg) {
        let text = self
            .pages
            .bluetooth
            .update(ui, msg, self.bluetooth.as_ref());
        say(&self.status, text);
    }
}
