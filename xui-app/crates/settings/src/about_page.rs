//! The About page: OS version, uptime, and where the settings are stored.

use std::rc::Rc;

use xui_core::app::Ui;
use xui_core::arrange::{button, column, grid, label, Handle, IntoEntry, LayoutExt, Mounted};
use xui_core::backend::{Result, WidgetId};
use xui_core::layout::{Align, Track};
use xui_core::widget::Label;
use xui_core::HasText;

use crate::app::Msg;
use crate::system::{StoreStatus, System};

/// `1 d 02:03:04`-style uptime text.
pub fn uptime_text(secs: u64) -> String {
    let (days, rest) = (secs / 86_400, secs % 86_400);
    let clock = format!(
        "{:02}:{:02}:{:02}",
        rest / 3600,
        (rest / 60) % 60,
        rest % 60
    );
    if days == 0 {
        clock
    } else {
        format!("{days} d {clock}")
    }
}

/// The persistence line for `status`.
pub fn persistence_text(status: Option<&StoreStatus>) -> String {
    match status {
        Some(status) if status.persistent => String::from("Settings survive a reboot (/conf)."),
        Some(_) => String::from("Settings are lost at reboot: /conf is not writable."),
        None => String::from("The configuration service is not running."),
    }
}

/// The page's widgets: captions beside their values.
pub struct AboutPage {
    version: Rc<Label<Msg>>,
    uptime: Rc<Label<Msg>>,
    store: Rc<Label<Msg>>,
    persistence: Rc<Label<Msg>>,
    _mounted: Mounted<Msg>,
}

impl AboutPage {
    /// Lays the page out in the container `page`.
    pub fn build(ui: &Ui<Msg>, page: WidgetId) -> Result<AboutPage> {
        let values: [Handle<Label<Msg>>; 4] = Default::default();
        let mut cells = Vec::new();
        for (caption, value) in ["Version", "Uptime", "Settings store", "Persistence"]
            .into_iter()
            .zip(&values)
        {
            cells.push(label(caption).into_entry());
            cells.push(label("").bind(value).into_entry());
        }
        let mounted = ui.mount_in(
            page,
            column().padding(20).gap(16).children((
                grid([Track::Auto, Track::Fill(1)]).gap(12).children(cells),
                button("Refresh")
                    .on_click(Msg::AboutRefresh)
                    .align(Align::Start),
            )),
        )?;
        let [version, uptime, store, persistence] = values;
        Ok(AboutPage {
            version: version.get(),
            uptime: uptime.get(),
            store: store.get(),
            persistence: persistence.get(),
            _mounted: mounted,
        })
    }

    /// Re-read every value.
    pub fn load(&self, system: &dyn System) {
        self.version.set_text(&system.os_version());
        self.uptime.set_text(
            &system
                .uptime_secs()
                .map_or_else(|| String::from("unknown"), uptime_text),
        );
        let status = system.store_status();
        self.store.set_text(
            status
                .as_ref()
                .map_or("unknown", |status| status.dir.as_str()),
        );
        self.persistence
            .set_text(&persistence_text(status.as_ref()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uptime_formats_days_only_when_needed() {
        assert_eq!(uptime_text(0), "00:00:00");
        assert_eq!(uptime_text(3_723), "01:02:03");
        assert_eq!(uptime_text(2 * 86_400 + 59), "2 d 00:00:59");
    }

    #[test]
    fn persistence_text_covers_every_state() {
        let mut status = StoreStatus {
            dir: "/conf".into(),
            persistent: true,
        };
        assert!(persistence_text(Some(&status)).contains("survive"));
        status.persistent = false;
        assert!(persistence_text(Some(&status)).contains("lost"));
        assert!(persistence_text(None).contains("not running"));
    }
}
