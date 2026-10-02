//! The Settings window: a vertical `IconView` of sections on the left, the
//! active section's page on the right.
//!
//! Each page is a [`Panel`]; switching sections shows one and hides the rest.
//! Colours are chosen with xui's own pickers: swatch grids ([`ColorPicker`])
//! for the quick accent and background choices, and the full [`ColorPanel`]
//! (HSV field, hue slider, HEX/RGB boxes) for any of the five themed colours.
//! Every change is written straight to the [`ConfigStore`] (confd on LazyOS),
//! where `xuid` and `inputd` pick it up live, and the status line reports the
//! outcome. The clock, the zone and the About facts go through [`System`].
//! The window itself follows the theme it edits ([`theme_ops::xui_theme_for`]).

use std::rc::Rc;

use uitheme::Mode;
use xui_core::app::{App, Ui};
use xui_core::backend::Result;
use xui_core::widget::{
    Button, CheckBox, ColorPanel, ColorPicker, Edit, IconView, Label, ListView, Panel, RadioGroup,
};
use xui_core::{Color, HasText, Rect};

use crate::about_page::AboutPage;
use crate::hidden_page::{HiddenMsg, HiddenPage};
use crate::keyboard;
use crate::menu_page::{MenuMsg, MenuPage};
use crate::sections::{Section, SectionsModel};
use crate::store::ConfigStore;
use crate::system::System;
use crate::theme_ops::{self, ACCENTS, BACKGROUNDS};
use crate::time_page::{TimeMsg, TimePage};

/// Window size (DIP) the app asks for: tall enough for every section row in
/// the sidebar without scrolling.
pub const WINDOW: (i32, i32) = (640, 500);
/// Width of the section sidebar.
const SIDEBAR_W: i32 = 150;
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
    appearance: Panel<Msg>,
    windows: Panel<Msg>,
    keyboard: Panel<Msg>,
    menu: MenuPage,
    hidden: HiddenPage,
    time: TimePage,
    about: AboutPage,
    mode: RadioGroup<Msg>,
    anim: CheckBox<Msg>,
    accent: ColorPicker<Msg>,
    background: ColorPicker<Msg>,
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
    pages: Pages,
    status: Label<Msg>,
    /// The Windows page's selected colour target.
    target: usize,
}

fn rect(x: i32, y: i32, w: i32, h: i32) -> Rect {
    Rect::new(x, y, x + w, y + h)
}

fn swatches<M: 'static>(
    ui: &Ui<M>,
    bounds: Rect,
    colors: impl Iterator<Item = u32>,
) -> Result<ColorPicker<M>> {
    let colors: Vec<Color> = colors.map(Color::hex).collect();
    Ok(ColorPicker::new(ui, bounds, &colors)?.columns(6))
}

impl SettingsApp {
    /// Builds the window's widgets over `store` and `system`.
    pub fn build(
        ui: &mut Ui<Msg>,
        store: Rc<dyn ConfigStore>,
        system: Rc<dyn System>,
    ) -> Result<SettingsApp> {
        let sidebar = IconView::with_model(ui, rect(0, 0, SIDEBAR_W, WINDOW.1), SectionsModel)?
            .multi_select(false)
            .on_select(|index| Some(Msg::Section(index)));
        sidebar.select(Some(0));
        ui.on_close(|| Some(Msg::Close));

        let page = rect(SIDEBAR_W, 0, WINDOW.0 - SIDEBAR_W, WINDOW.1 - STATUS_H);
        let mut labels = Vec::new();
        let mut buttons = Vec::new();

        let appearance = Panel::new(ui, page)?;
        let (mode, accent, background, anim) = {
            let p = appearance.ui();
            labels.push(Label::new(p, rect(20, 14, 200, 20), "Theme")?);
            let mode = RadioGroup::new(p, rect(20, 38, 200, 52), &["Dark", "Light"])?
                .on_select(|i| Some(Msg::Mode(i)));
            labels.push(Label::new(p, rect(20, 112, 300, 20), "Accent color")?);
            let accent = swatches(p, rect(20, 136, 260, 40), ACCENTS.iter().map(|a| a.1))?
                .on_select(|c| Some(Msg::Accent(pack(c))));
            labels.push(Label::new(p, rect(20, 196, 300, 20), "Desktop background")?);
            let background = swatches(p, rect(20, 220, 260, 40), BACKGROUNDS.iter().map(|b| b.1))?
                .on_select(|c| Some(Msg::Background(pack(c))));
            let anim = CheckBox::new(p, rect(20, 268, 260, 24), "Window animations")?
                .on_toggle(|on| Some(Msg::Anim(on)));
            buttons.push(
                Button::new(p, rect(20, 300, 170, 30), "Reset to defaults")?
                    .on_click(|| Some(Msg::ResetAppearance)),
            );
            (mode, accent, background, anim)
        };

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
            pages: Pages {
                appearance,
                windows,
                keyboard,
                menu,
                hidden,
                time,
                about,
                mode,
                anim,
                accent,
                background,
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
        };
        app.show(Section::Appearance);
        app.load_state();
        app.retheme(ui);
        if !app.store.persistent() {
            app.status
                .set_text("Settings are not persistent: no data volume is mounted.");
        }
        Ok(app)
    }

    /// Show `section`'s page and hide the others. The Time & Date, Hidden
    /// apps and About pages show live values, so they are re-read each time
    /// they appear.
    fn show(&mut self, section: Section) {
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
        p.mode.select(usize::from(settings.mode == Mode::Light));
        p.anim.set_checked(settings.anim);
        // A custom colour matches no swatch, which clears the selection.
        let accent = settings.accent.unwrap_or(uitheme::DEFAULT_ACCENT);
        p.accent.select(Color::hex(accent));
        p.background
            .select(Color::hex(settings.bg.unwrap_or(u32::MAX)));
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
                self.report(
                    theme_ops::set_color(store, uitheme::KEY_BG, Some(rgb)),
                    "Background changed.",
                );
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
                self.pages.accent.select(Color::hex(
                    settings.accent.unwrap_or(uitheme::DEFAULT_ACCENT),
                ));
                self.pages
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
