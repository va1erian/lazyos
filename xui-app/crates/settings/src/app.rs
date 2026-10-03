//! The Settings window: a vertical `IconView` of sections on the left, the
//! active section's page on the right.
//!
//! Each page is a [`Panel`]; switching sections shows one and hides the rest.
//! Colours are chosen with xui's own pickers: swatch grids ([`ColorPicker`])
//! for the quick accent and background choices, and the full [`ColorPanel`]
//! (HSV field, hue slider, HEX/RGB boxes) for any of the five themed colours.
//! The Appearance page also lists the desktop pictures ([`wallpaper_ops`]).
//! Every change is written straight to the [`ConfigStore`] (confd on LazyOS),
//! where `xuid` and `inputd` pick it up live, and the status line reports the
//! outcome. The clock, the zone and the About facts go through [`System`].
//! The window itself follows the theme it edits ([`theme_ops::xui_theme_for`]).

use std::rc::Rc;

use uitheme::Mode;
use xui_core::app::{App, Ui};
use xui_core::backend::Result;
use xui_core::backend::{NodeKind, NodeSpec};
use xui_core::widget::{
    Button, ColorPanel, Control, Edit, IconSize, IconView, Label, ListView, Panel,
};
use xui_core::{Color, HasText, Point, Rect, Rgba};

use crate::about_page::AboutPage;
use crate::appearance_page::AppearancePage;
use crate::hidden_page::{HiddenMsg, HiddenPage};
use crate::keyboard;
use crate::menu_page::{MenuMsg, MenuPage};
use crate::sections::{Section, SectionsModel};
use crate::store::ConfigStore;
use crate::system::System;
use crate::theme_ops;
use crate::time_page::{TimeMsg, TimePage};
use crate::wallpaper_ops;

/// Window size (DIP) the app asks for: tall enough for every section row in
/// the sidebar without scrolling.
pub const WINDOW: (i32, i32) = (660, 560);
/// Width of the section sidebar.
const SIDEBAR_W: i32 = 150;
/// Top of the pages, under the page title.
const PAGE_TOP: i32 = 56;
/// Height reserved under the pages for the status line.
const STATUS_H: i32 = 28;

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

/// A `Color` as `0xRRGGBB`.
fn pack(color: Color) -> u32 {
    (u32::from(color.r) << 16) | (u32::from(color.g) << 8) | u32::from(color.b)
}

/// The pages' widgets, kept alive for the life of the window.
struct Pages {
    appearance: AppearancePage,
    windows: Panel<Msg>,
    keyboard: Panel<Msg>,
    menu: MenuPage,
    hidden: HiddenPage,
    time: TimePage,
    about: AboutPage,
    _target: ListView<Msg>,
    panel: ColorPanel<Msg>,
    layout: ListView<Msg>,
    layout_hint: Label<Msg>,
    // Owned only to keep their nodes registered.
    _labels: Vec<Label<Msg>>,
    _buttons: Vec<Button<Msg>>,
    _test: Edit<Msg>,
}

/// The Settings app.
pub struct SettingsApp {
    store: Rc<dyn ConfigStore>,
    system: Rc<dyn System>,
    _sidebar: IconView<Msg>,
    _sidebar_back: Control<Msg>,
    /// The page title: the selected section's name.
    title: Label<Msg>,
    pages: Pages,
    status: Label<Msg>,
    /// The Windows page's selected colour target.
    target: usize,
    /// The desktop pictures listed, in row order after "None".
    pictures: Vec<String>,
}

fn rect(x: i32, y: i32, w: i32, h: i32) -> Rect {
    crate::layout::rect(x, y, w, h)
}

impl SettingsApp {
    /// Builds the window's widgets over `store` and `system`.
    pub fn build(
        ui: &mut Ui<Msg>,
        store: Rc<dyn ConfigStore>,
        system: Rc<dyn System>,
    ) -> Result<SettingsApp> {
        crate::layout::set_dpi(ui.dpi());
        let sidebar_back = sidebar_backdrop(ui)?;
        let sidebar = IconView::with_model(
            ui,
            rect(6, 10, SIDEBAR_W - 12, WINDOW.1 - 20),
            SectionsModel,
        )?
        .multi_select(false)
        .on_select(|index| Some(Msg::Section(index)));
        sidebar.set_icon_size(IconSize::Medium);
        sidebar.select(Some(0));
        ui.on_close(|| Some(Msg::Close));

        let title = Label::new(ui, rect(SIDEBAR_W + 18, 10, 400, 38), "")?.title();
        // A card per page; Appearance draws its own cards on the window.
        let page = rect(
            SIDEBAR_W + 16,
            PAGE_TOP,
            WINDOW.0 - SIDEBAR_W - 32,
            WINDOW.1 - STATUS_H - PAGE_TOP - 8,
        );
        let mut labels = Vec::new();
        let mut buttons = Vec::new();

        let pictures = system.wallpapers();
        let appearance = AppearancePage::build(
            ui,
            rect(
                SIDEBAR_W,
                PAGE_TOP,
                WINDOW.0 - SIDEBAR_W,
                WINDOW.1 - STATUS_H - PAGE_TOP,
            ),
            &pictures,
        )?;

        let windows = Panel::new(ui, page)?;
        let (target, panel) = {
            let p = windows.ui();
            labels.push(Label::new(p, rect(20, 14, 160, 20), "Color to change")?);
            let names: Vec<&str> = TARGETS.iter().map(|(n, _)| *n).collect();
            let target = ListView::new(p, rect(20, 38, 160, 150), &names)?
                .multi_select(false)
                .on_select(|i| Some(Msg::Target(i)));
            buttons.push(
                Button::new(p, rect(20, 200, 160, 30), "Use default")?
                    .on_click(|| Some(Msg::UseDefault)),
            );
            let panel = ColorPanel::new(p, rect(196, 14, 476 - 196, 360))?
                .on_commit(|c| Some(Msg::Commit(pack(c))));
            (target, panel)
        };
        target.select(Some(0));

        let keyboard = Panel::new(ui, page)?;
        let (layout, layout_hint, test) = {
            let p = keyboard.ui();
            labels.push(Label::new(p, rect(20, 14, 300, 20), "Keyboard layout")?);
            let names: Vec<&str> = keyboard::LAYOUTS.iter().map(|(_, n)| *n).collect();
            let layout = ListView::new(p, rect(20, 38, 280, 60), &names)?
                .multi_select(false)
                .on_select(|i| Some(Msg::Layout(i)));
            let hint = Label::new(p, rect(20, 108, 400, 20), "")?;
            labels.push(Label::new(p, rect(20, 150, 300, 20), "Try it")?);
            let test =
                Edit::new(p, rect(20, 174, 280, 26), "")?.cue("Type here to test the layout");
            (layout, hint, test)
        };

        let menu = MenuPage::build(ui, page)?;
        let hidden = HiddenPage::build(ui, page)?;
        let time = TimePage::build(ui, page)?;
        let about = AboutPage::build(ui, page)?;

        let status = Label::new(
            ui,
            rect(SIDEBAR_W + 20, WINDOW.1 - STATUS_H + 4, 460, 20),
            "",
        )?;

        let mut app = SettingsApp {
            store,
            system,
            _sidebar: sidebar,
            _sidebar_back: sidebar_back,
            title,
            pages: Pages {
                appearance,
                windows,
                keyboard,
                menu,
                hidden,
                time,
                about,
                _target: target,
                panel,
                layout,
                layout_hint,
                _labels: labels,
                _buttons: buttons,
                _test: test,
            },
            status,
            target: 0,
            pictures,
        };
        app.show(Section::Appearance);
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
    fn show(&mut self, section: Section) {
        self.title.set_text(section.label());
        let p = &mut self.pages;
        p.appearance.set_visible(section == Section::Appearance);
        p.windows.set_visible(section == Section::Windows);
        p.time.set_visible(section == Section::Time);
        p.keyboard.set_visible(section == Section::Keyboard);
        p.menu.set_visible(section == Section::Menu);
        p.hidden.set_visible(section == Section::Hidden);
        p.about.set_visible(section == Section::About);
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
        a.mode.select(usize::from(settings.mode == Mode::Light));
        a.anim.set_checked(settings.anim);
        // A custom colour matches no swatch, which clears the selection.
        let accent = settings.accent.unwrap_or(uitheme::DEFAULT_ACCENT);
        a.accent.select(Color::hex(accent));
        a.background
            .select(Color::hex(settings.bg.unwrap_or(u32::MAX)));
        let picture = wallpaper_ops::current(self.store.as_ref());
        a.wallpaper
            .select(wallpaper_ops::row_of(&self.pictures, picture.as_deref()));
        match keyboard::current(self.store.as_ref()) {
            Some(i) => {
                p.layout.select(Some(i));
                p.layout_hint
                    .set_text(&format!("Active: {}", keyboard::LAYOUTS[i].1));
            }
            None => p
                .layout_hint
                .set_text("No layout chosen yet (using the boot default)."),
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
        self.pages.panel.set_color(Color::hex(rgb));
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
                    self.show(section);
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
                self.pages.appearance.accent.select(Color::hex(
                    settings.accent.unwrap_or(uitheme::DEFAULT_ACCENT),
                ));
                self.pages
                    .appearance
                    .background
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
                if let Some((_, name)) = keyboard::LAYOUTS.get(i) {
                    self.pages.layout_hint.set_text(&format!("Active: {name}"));
                }
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

#[cfg(test)]
mod tests {
    use super::*;

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

/// The sidebar's own background: the window darkened, with a hairline on its
/// right edge, so the section list reads as a column apart from the page.
fn sidebar_backdrop(ui: &Ui<Msg>) -> Result<Control<Msg>> {
    let back = Control::new(
        ui,
        &NodeSpec::new(NodeKind::Custom, rect(0, 0, SIDEBAR_W, WINDOW.1)),
    )?;
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
