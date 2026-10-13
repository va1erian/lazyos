//! The About page: OS version, uptime, where the settings are stored, and
//! credits for the third-party icons the desktop draws.

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

/// Credits shown under the values, one line each. The Lucide outlines are
/// drawn by every app and the shell; their licences require the full notice
/// with every copy, which the image ships at [`fhs::share::LUCIDE_LICENSE`].
pub fn credits() -> [String; 3] {
    [
        String::from("Icons: Lucide (lucide.dev), \u{a9} Lucide Contributors, ISC License."),
        String::from("Lucide includes icons from Feather, \u{a9} Cole Bemis, MIT License."),
        format!("Full licence text: {}", fhs::share::LUCIDE_LICENSE),
    ]
}

/// The page's widgets: captions beside their values.
pub struct AboutPage {
    version: Rc<Label<Msg>>,
    revision: Rc<Label<Msg>>,
    image: Rc<Label<Msg>>,
    uptime: Rc<Label<Msg>>,
    store: Rc<Label<Msg>>,
    persistence: Rc<Label<Msg>>,
    _mounted: Mounted<Msg>,
}

impl AboutPage {
    /// Lays the page out in the container `page`.
    pub fn build(ui: &Ui<Msg>, page: WidgetId) -> Result<AboutPage> {
        let values: [Handle<Label<Msg>>; 6] = Default::default();
        let mut cells = Vec::new();
        for (caption, value) in [
            "Version",
            "Revision",
            "Image",
            "Uptime",
            "Settings store",
            "Persistence",
        ]
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
                column().gap(4).children(
                    credits()
                        .into_iter()
                        .map(|line| label(&line).into_entry())
                        .collect::<Vec<_>>(),
                ),
            )),
        )?;
        let [version, revision, image, uptime, store, persistence] = values;
        Ok(AboutPage {
            version: version.get(),
            revision: revision.get(),
            image: image.get(),
            uptime: uptime.get(),
            store: store.get(),
            persistence: persistence.get(),
            _mounted: mounted,
        })
    }

    /// Re-read every value.
    pub fn load(&self, system: &dyn System) {
        self.version.set_text(&system.os_version());
        self.revision.set_text(
            &system
                .os_revision()
                .unwrap_or_else(|| String::from("unknown")),
        );
        self.image
            .set_text(&system.os_image().unwrap_or_else(|| String::from("none")));
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

    #[test]
    fn the_image_id_is_the_proc_version_suffix_the_kernel_writes() {
        use crate::system::image_from_proc_version as image;
        let line = "LazyOS version 0.1.0 (x86_64) Linux-compatible ABI image 20261012-093000-01234567-dirty\n";
        assert_eq!(image(line).as_deref(), Some("20261012-093000-01234567-dirty"));
        // A build with no id has no suffix; nothing else is taken for one.
        assert_eq!(image("LazyOS version 0.1.0 (x86_64) Linux-compatible ABI\n"), None);
        assert_eq!(image(""), None);
        assert_eq!(image("x image \n"), None);
        assert_eq!(image(&format!("x image {}", "a".repeat(65))), None);
        assert_eq!(image("x image has space"), None);
        assert_eq!(image(&format!("x image {}", "a".repeat(64))).map(|id| id.len()), Some(64));
    }

    #[test]
    fn credits_name_lucide_and_its_licences() {
        let text = credits().join(" ");
        assert!(text.contains("Lucide Contributors"));
        assert!(text.contains("ISC License"));
        assert!(text.contains("Feather"));
        assert!(text.contains(fhs::share::LUCIDE_LICENSE));
    }

    /// The notice the credits point to is shipped by the image: the asset
    /// exists, carries both licences, and the manifest installs it at the
    /// path the page shows.
    #[test]
    fn the_licence_notice_ships_where_the_credits_point() {
        let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../assets/");
        let notice = std::fs::read_to_string(format!("{root}licenses/LICENSE-Lucide.txt")).unwrap();
        assert!(notice.contains("ISC License") && notice.contains("Cole Bemis"));
        let manifest = std::fs::read_to_string(format!("{root}manifest.txt")).unwrap();
        assert!(manifest.contains("licenses/LICENSE-Lucide.txt | ISC | all |"));
        let installed = fhs::share::LUCIDE_LICENSE.strip_prefix(fhs::SYSTEM_SHARE);
        assert_eq!(installed, Some("/licenses/LICENSE-Lucide.txt"));
    }
}
