//! The About page: OS version, uptime, and where the settings are stored.

use xui_core::app::Ui;
use xui_core::backend::Result;
use xui_core::widget::{Button, Label, Panel};
use xui_core::{HasText, Rect};

use crate::app::Msg;
use crate::system::{StoreStatus, System};

fn rect(x: i32, y: i32, w: i32, h: i32) -> Rect {
    Rect::new(x, y, x + w, y + h)
}

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
        Some(status) if status.persistent => {
            String::from("Settings survive a reboot (/conf).")
        }
        Some(_) => String::from("Settings are lost at reboot: /conf is not writable."),
        None => String::from("The configuration service is not running."),
    }
}

/// The page's widgets: a fixed column of captions and their values.
pub struct AboutPage {
    panel: Panel<Msg>,
    version: Label<Msg>,
    uptime: Label<Msg>,
    store: Label<Msg>,
    persistence: Label<Msg>,
    _labels: Vec<Label<Msg>>,
    _refresh: Button<Msg>,
}

impl AboutPage {
    /// Build the page (hidden state is the caller's job) inside `bounds`.
    pub fn build(ui: &Ui<Msg>, bounds: Rect) -> Result<AboutPage> {
        let panel = Panel::new(ui, bounds)?;
        let p = panel.ui();
        let caption = |y, text| Label::new(p, rect(20, y, 130, 20), text);
        let value = |y| Label::new(p, rect(150, y, 320, 20), "");
        let labels = vec![
            caption(14, "Version")?,
            caption(44, "Uptime")?,
            caption(74, "Settings store")?,
            caption(104, "Persistence")?,
        ];
        let page = AboutPage {
            version: value(14)?,
            uptime: value(44)?,
            store: value(74)?,
            persistence: Label::new(p, rect(150, 104, 320, 40), "")?,
            _labels: labels,
            _refresh: Button::new(p, rect(20, 150, 100, 28), "Refresh")?
                .on_click(|| Some(Msg::AboutRefresh)),
            panel,
        };
        Ok(page)
    }

    pub fn set_visible(&self, visible: bool) {
        self.panel.set_visible(visible);
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
