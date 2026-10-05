//! The Settings window: a vertical `IconView` of sections on the left, the
//! active section's page on the right.
//!
//! The window is one layout: the sidebar, then the page title, a container per
//! section and the status line. Each page lays its widgets out in its own
//! container; switching sections shows one container and hides the rest.
//! Colours are chosen with xui's own pickers: swatch grids (`ColorPicker`)
//! for the quick accent and background choices, and the full `ColorPanel`
//! (HSV field, hue slider, HEX/RGB boxes) for any of the five themed colours.
//! The Appearance page also lists the desktop pictures ([`wallpaper_ops`]).
//! Every change is written straight to the [`ConfigStore`] (confd on LazyOS),
//! where `xuid` and `inputd` pick it up live, and the status line reports the
//! outcome. The clock, the zone and the About facts go through [`System`].
//! The window itself follows the theme it edits ([`theme_ops::xui_theme_for`]).

use std::rc::Rc;

use uitheme::Mode;
use xui_core::app::{App, Ui};
use xui_core::arrange::{build, column, label, row, Handle, LayoutExt, Mounted};
use xui_core::backend::{NodeKind, NodeSpec, Result};
use xui_core::layout::Insets;
use xui_core::widget::{Control, IconSize, IconView, Label, Panel};
use xui_core::{Color, Dip, HasText, Point, Rect, Rgba};

use crate::about_page::AboutPage;
use crate::appearance_page::AppearancePage;
use crate::hidden_page::{HiddenMsg, HiddenPage};
use crate::keyboard;
use crate::keyboard_page::KeyboardPage;
use crate::menu_page::{MenuMsg, MenuPage};
use crate::place::{placed, Placed};
use crate::sections::{Section, SectionsModel};
use crate::store::ConfigStore;
use crate::system::System;
use crate::theme_ops;
use crate::time_page::{TimeMsg, TimePage};
use crate::wallpaper_ops;
use crate::windows_page::WindowsPage;

/// Window size (DIP) the app asks for: tall enough for every section row in
/// the sidebar without scrolling.
pub const WINDOW: (i32, i32) = (660, 560);
/// Width of the section sidebar.
const SIDEBAR_W: i32 = 150;
/// Height of the page title, above the pages.
const TITLE_H: i32 = 38;
/// Height of the status line, under the pages.
const STATUS_H: i32 = 20;

/// The colours the Windows page can edit: label, confd key.
const TARGETS: [(&str, &str); 5] = [
    ("Desktop background", uitheme::KEY_BG),
    ("Active title bar", uitheme::KEY_TITLE_ACTIVE),
    ("Inactive title bar", uitheme::KEY_TITLE_INACTIVE),
    ("Taskbar", uitheme::KEY_TASKBAR),
    ("Accent", uitheme::KEY_ACCENT),
];

/// Messages the widgets raise.
#[derive(Clone, Debug, PartialEq)]
pub enum Msg {
    Section(usize),
    Mode(usize),
    /// A swatch was picked: `0xRRGGBB`.
    Accent(u32),
    Background(u32),
    /// A desktop picture row was picked (0: none).
    Wallpaper(usize),
    /// The animations checkbox changed.
    Anim(bool),
    ResetAppearance,
    Target(usize),
    /// The colour panel committed `0xRRGGBB` for the selected target.
    Commit(u32),
    UseDefault,
    Layout(usize),
    Menu(MenuMsg),
    Hidden(HiddenMsg),
    Time(TimeMsg),
    AboutRefresh,
    /// The compositor asked the window to close.
    Close,
}

/// The pages, kept alive for the life of the window.
struct Pages {
    appearance: AppearancePage,
    windows: WindowsPage,
    keyboard: KeyboardPage,
    menu: MenuPage,
    hidden: HiddenPage,
    time: TimePage,
    about: AboutPage,
}

/// The Settings app.
pub struct SettingsApp {
    store: Rc<dyn ConfigStore>,
    system: Rc<dyn System>,
    /// The page title: the selected section's name.
    title: Rc<Label<Msg>>,
    /// One container per section, in [`Section::ALL`] order.
    frames: Vec<Rc<Placed<Panel<Msg>>>>,
    pages: Pages,
    status: Rc<Label<Msg>>,
    _sidebar: Mounted<Msg>,
    /// The Windows page's selected colour target.
    target: usize,
    /// The desktop pictures listed, in row order after "None".
    pictures: Vec<String>,
}

impl SettingsApp {
    /// Builds the window's widgets over `store` and `system`.
    pub fn build(
        ui: &mut Ui<Msg>,
        store: Rc<dyn ConfigStore>,
        system: Rc<dyn System>,
    ) -> Result<SettingsApp> {
        ui.on_close(|| Some(Msg::Close));
        let (sidebar, title, status) = (Handle::new(), Handle::new(), Handle::new());
        let frames: Vec<Handle<Placed<Panel<Msg>>>> =
            Section::ALL.iter().map(|_| Handle::new()).collect();

        let mut main = vec![label("").title().bind(&title).fixed(TITLE_H)];
        for (section, frame) in Section::ALL.into_iter().zip(&frames) {
            // Appearance draws its own sections on the window; the others
            // sit on a card.
            let card = section != Section::Appearance;
            let panel = placed(0, 0, move |ui, bounds| {
                if card {
                    Panel::new(ui, bounds)
                } else {
                    Panel::plain(ui, bounds)
                }
            });
            main.push(panel.bind(frame).fill(1));
        }
        main.push(
            row()
                .padding(Insets::new(Dip(4.0), Dip(0.0), Dip(0.0), Dip(0.0)))
                .child(label("").bind(&status).fill(1))
                .fixed(STATUS_H),
        );
        ui.root(
            row().children((
                placed(SIDEBAR_W, 0, sidebar_backdrop)
                    .bind(&sidebar)
                    .width(SIDEBAR_W),
                column()
                    .padding(Insets::new(Dip(16.0), Dip(10.0), Dip(16.0), Dip(8.0)))
                    .gap(8)
                    .children(main)
                    .fill(1),
            )),
        )?;
        let sidebar = ui.mount_in(
            sidebar.get().widget.id(),
            column()
                .padding(Insets::symmetric(Dip(6.0), Dip(10.0)))
                .child(
                    build(|ui| {
                        let view = IconView::with_model(ui, Rect::default(), SectionsModel)?
                            .multi_select(false)
                            .on_select(|index| Some(Msg::Section(index)));
                        view.set_icon_size(IconSize::Medium);
                        view.select(Some(0));
                        Ok(view)
                    })
                    .fill(1),
                ),
        )?;

        let frames: Vec<Rc<Placed<Panel<Msg>>>> = frames.iter().map(Handle::get).collect();
        let page = |section: Section| frames[section.index()].widget.id();
        let pictures = system.wallpapers();
        let names: Vec<&'static str> = TARGETS.iter().map(|(n, _)| *n).collect();
        let pages = Pages {
            appearance: AppearancePage::build(ui, page(Section::Appearance), &pictures)?,
            windows: WindowsPage::build(ui, page(Section::Windows), names)?,
            keyboard: KeyboardPage::build(ui, page(Section::Keyboard))?,
            menu: MenuPage::build(ui, page(Section::Menu))?,
            hidden: HiddenPage::build(ui, page(Section::Hidden))?,
            time: TimePage::build(ui, page(Section::Time))?,
            about: AboutPage::build(ui, page(Section::About))?,
        };

        let mut app = SettingsApp {
            store,
            system,
            title: title.get(),
            frames,
            pages,
            status: status.get(),
            _sidebar: sidebar,
            target: 0,
            pictures,
        };
        app.show(ui, Section::Appearance);
        app.load_state();
        app.retheme(ui);
        if !app.store.persistent() {
            app.status
                .set_text("Settings are not persistent: /conf is not writable.");
        }
        Ok(app)
    }

    /// Show `section`'s page and hide the others. The Time & Date, Hidden
    /// apps and About pages show live values, so they are re-read each time
    /// they appear.
    fn show(&mut self, ui: &Ui<Msg>, section: Section) {
        self.title.set_text(section.label());
        for (index, frame) in self.frames.iter().enumerate() {
            ui.set_visible(frame.widget.id(), index == section.index());
        }
        let p = &mut self.pages;
        match section {
            Section::Time => p.time.load(self.store.as_ref(), self.system.as_ref()),
            Section::About => p.about.load(self.system.as_ref()),
            Section::Hidden => p.hidden.load(self.store.as_ref()),
            _ => {}
        }
    }

    /// Match this window's widgets to the stored desktop theme.
    fn retheme(&self, ui: &Ui<Msg>) {
        let settings = theme_ops::load(self.store.as_ref());
        ui.set_theme(theme_ops::xui_theme_for(&settings));
    }

    /// Point every control at the stored values (no events are raised).
    fn load_state(&mut self) {
        let settings = theme_ops::load(self.store.as_ref());
        let p = &self.pages;
        let a = &p.appearance;
        a.mode
            .widget
            .select(usize::from(settings.mode == Mode::Light));
        a.anim.set_checked(settings.anim);
        // A custom colour matches no swatch, which clears the selection.
        let accent = settings.accent.unwrap_or(uitheme::DEFAULT_ACCENT);
        a.accent.widget.select(Color::hex(accent));
        a.background
            .widget
            .select(Color::hex(settings.bg.unwrap_or(u32::MAX)));
        let picture = wallpaper_ops::current(self.store.as_ref());
        a.wallpaper
            .select(wallpaper_ops::row_of(&self.pictures, picture.as_deref()));
        match keyboard::current(self.store.as_ref()) {
            Some(i) => p.keyboard.show_layout(i),
            None => p.keyboard.show_default(),
        }
        self.load_target(self.target);
        self.pages.menu.load(self.store.as_ref());
    }

    /// Point the colour panel at the colour in effect for `target`.
    fn load_target(&mut self, target: usize) {
        self.target = target;
        let settings = theme_ops::load(self.store.as_ref());
        let palette = uitheme::resolve(&settings);
        let rgb = match target {
            0 => palette.background,
            1 => palette.title_bg_focus,
            2 => palette.title_bg,
            3 => palette.taskbar_bg,
            _ => palette.taskbar_entry_focus,
        };
        self.pages.windows.set_color(rgb);
    }

    fn report(&self, result: std::result::Result<(), String>, ok: &str) {
        match result {
            Ok(()) => self.status.set_text(ok),
            Err(error) => self.status.set_text(&format!("Could not save: {error}")),
        }
    }
}

impl App for SettingsApp {
    type Msg = Msg;

    fn update(&mut self, msg: Msg, ui: &mut Ui<Msg>) {
        // Serial evidence that input reached the app (see `xui_settings.json`).
        println!("SETTINGS:MSG:{msg:?}");
        let store = Rc::clone(&self.store);
        let store = store.as_ref();
        match msg {
            Msg::Close => {
                println!("SETTINGS:CLOSE:PASS");
                ui.quit();
            }
            Msg::Section(i) => {
                if let Some(section) = Section::from_index(i) {
                    self.show(ui, section);
                }
            }
            Msg::Mode(i) => {
                let mode = if i == 1 { Mode::Light } else { Mode::Dark };
                self.report(theme_ops::set_mode(store, mode), "Theme changed.");
                // A mode change clears the overrides: show the presets again.
                self.load_state();
                self.retheme(ui);
            }
            Msg::Accent(rgb) => {
                self.report(
                    theme_ops::set_color(store, uitheme::KEY_ACCENT, Some(rgb)),
                    "Accent color changed.",
                );
                self.retheme(ui);
            }
            Msg::Background(rgb) => {
                // A picture would hide the colour just chosen: drop it.
                let result = theme_ops::set_color(store, uitheme::KEY_BG, Some(rgb))
                    .and_then(|()| wallpaper_ops::set(store, None));
                self.report(result, "Background changed.");
                // Show what is stored: "None" on success, and after a failed
                // write the picture (and swatch) still in effect.
                self.load_state();
            }
            Msg::Wallpaper(row) => {
                let result = wallpaper_ops::choose(store, &self.pictures, row);
                let failed = result.is_err();
                let text = result.as_ref().map_or("", |text| *text);
                self.report(result.map(|_| ()), text);
                if failed {
                    // The clicked row was not saved: select the stored one.
                    self.load_state();
                }
            }
            Msg::Anim(on) => self.report(
                theme_ops::set_animations(store, on),
                if on {
                    "Animations on."
                } else {
                    "Animations off."
                },
            ),
            Msg::ResetAppearance => {
                self.report(theme_ops::reset(store), "Appearance reset to defaults.");
                self.load_state();
                self.retheme(ui);
            }
            Msg::Target(i) => self.load_target(i.min(TARGETS.len() - 1)),
            Msg::Commit(rgb) => {
                let (name, key) = TARGETS[self.target];
                self.report(
                    theme_ops::set_color(store, key, Some(rgb)),
                    &format!("{name} changed."),
                );
                // Keep the Appearance swatches in step with the new override.
                let settings = theme_ops::load(store);
                let a = &self.pages.appearance;
                a.accent.widget.select(Color::hex(
                    settings.accent.unwrap_or(uitheme::DEFAULT_ACCENT),
                ));
                a.background
                    .widget
                    .select(Color::hex(settings.bg.unwrap_or(u32::MAX)));
                self.retheme(ui);
            }
            Msg::UseDefault => {
                let (name, key) = TARGETS[self.target];
                self.report(
                    theme_ops::set_color(store, key, None),
                    &format!("{name} reset to default."),
                );
                self.load_state();
                self.retheme(ui);
            }
            Msg::Layout(i) => {
                self.report(keyboard::set(store, i), "Keyboard layout changed.");
                self.pages.keyboard.show_active(i);
            }
            Msg::Time(msg) => {
                let text = self.pages.time.update(msg, store, self.system.as_ref());
                if !text.is_empty() {
                    self.status.set_text(&text);
                }
            }
            Msg::AboutRefresh => self.pages.about.load(self.system.as_ref()),
            Msg::Menu(msg) => {
                let text = self.pages.menu.update(msg, store);
                if !text.is_empty() {
                    self.status.set_text(&text);
                }
            }
            Msg::Hidden(msg) => {
                let text = self.pages.hidden.update(msg, store);
                if !text.is_empty() {
                    self.status.set_text(&text);
                }
            }
        }
    }
}

/// The sidebar's own background, laid out at `bounds`: the window darkened,
/// with a hairline on its right edge, so the section list reads as a column
/// apart from the page. A container, so the section list sits inside it.
fn sidebar_backdrop(ui: &Ui<Msg>, bounds: Rect) -> Result<Control<Msg>> {
    let back = Control::new(ui, &NodeSpec::new(NodeKind::Container, bounds))?;
    let theme = ui.theme_handle();
    back.set_painter(Rc::new(move |canvas| {
        let theme = theme.get();
        let b = canvas.bounds();
        xui_core::theme::look::backdrop(canvas, theme.background);
        let shade = if theme.is_dark { 0x40 } else { 0x0C };
        canvas.fill_rect_rgba(b, Rgba::with_alpha(0, 0, 0, shade));
        canvas.draw_line(
            Point::new(b.right - 1, b.top),
            Point::new(b.right - 1, b.bottom),
            theme.border,
            1.0,
        );
    }));
    Ok(back)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::appearance_page::pack;

    #[test]
    fn pack_is_the_inverse_of_hex() {
        for rgb in [0x000000, 0xFFFFFF, 0x336699, 0x0E1C3C] {
            assert_eq!(pack(Color::hex(rgb)), rgb);
        }
    }

    #[test]
    fn every_target_key_is_a_theme_color_key() {
        for (_, key) in TARGETS {
            assert!(uitheme::COLOR_KEYS.contains(&key), "{key}");
        }
    }
}
