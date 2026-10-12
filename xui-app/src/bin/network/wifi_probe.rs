//! The UI probe lines of the Wi-Fi page (`LAZYOS_UI_PROBE=1` images): where
//! its controls and rows are, for `click_at` by name in a session script.

use xui_app::probe;
use xui_core::backend::WidgetId;

use super::wifi_page::View;
use super::{Network, TITLE};

impl Network {
    /// Print where the page's controls and rows are (UI probe images only).
    pub(super) fn wifi_probe(&self) {
        let w = &self.w.wifi;
        let ui = &self.ui;
        let rect = |name: &str, id: WidgetId| {
            let r = ui.bounds(id);
            probe::widget(
                TITLE,
                name,
                r.left,
                r.top,
                r.right - r.left,
                r.bottom - r.top,
            );
        };
        match self.wifi.view {
            View::Wired => {}
            View::List => {
                let buttons = [
                    ("wifi_connect", w.connect.get().id()),
                    ("wifi_disconnect", w.disconnect.get().id()),
                    ("wifi_forget", w.forget.get().id()),
                    ("wifi_scan", w.scan.get().id()),
                    ("wifi_other", w.other.get().id()),
                    ("wifi_back", w.back.get().id()),
                ];
                for (name, id) in buttons {
                    rect(name, id);
                }
                self.wifi_probe_rows();
                println!("NETAPP:WIFI:PROBE:list");
            }
            View::Join => {
                if let Some(kind) = &self.wifi.join {
                    w.join.probe(kind, |name, id| rect(name, id));
                }
                println!("NETAPP:WIFI:PROBE:join");
            }
        }
    }

    /// One `wifi_row:<name>` rectangle per listed network, from the list's
    /// header and row heights (24 and 22 design units).
    fn wifi_probe_rows(&self) {
        let r = self.ui.bounds(self.w.wifi.list.get().id());
        let dpi = self.ui.dpi() as i32;
        let (header, row) = (24 * dpi / 96, 22 * dpi / 96);
        for (index, name) in self.wifi.probed_rows.iter().enumerate() {
            let top = r.top + header + row * index as i32;
            probe::widget(
                TITLE,
                &format!("wifi_row:{name}"),
                r.left,
                top,
                r.right - r.left,
                row,
            );
        }
    }
}
