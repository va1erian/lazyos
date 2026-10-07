//! The Appearance page: the theme, accent and wallpaper (colour or one of the
//! desktop pictures, [`wallpaper_ops`]) choices, each a titled section, on the
//! window itself rather than on a card of its own
//! (docs/xui-theme-proposals.md, Midnight).
//!
//! Every choice here is the user's own theme ([`crate::user_theme`]); **Make
//! this the default for everyone** publishes it as the machine default the
//! login screen and other accounts follow, which asks an administrator.

use std::rc::Rc;

use xui_core::app::Ui;
use xui_core::arrange::{
    button, checkbox, color_picker, column, group, radio_group, row, Build, Handle, LayoutExt,
    Mounted,
};
use xui_core::backend::{Result, WidgetId};
use xui_core::layout::Align;
use xui_core::widget::{CheckBox, ColorPicker, ListView, RadioGroup};
use xui_core::Color;

use crate::app::{choice_list, Msg};
use crate::theme_ops::{ACCENTS, BACKGROUNDS};
use crate::wallpaper_ops;

/// A `Color` as `0xRRGGBB`.
pub fn pack(color: Color) -> u32 {
    (u32::from(color.r) << 16) | (u32::from(color.g) << 8) | u32::from(color.b)
}

/// The size a swatch grid is laid out at: one row of up to six swatches.
const SWATCHES: (i32, i32) = (300, 48);

/// The page's widgets.
pub struct AppearancePage {
    pub mode: Rc<RadioGroup<Msg>>,
    pub accent: Rc<ColorPicker<Msg>>,
    pub background: Rc<ColorPicker<Msg>>,
    /// The desktop pictures: "None", then one row per picture.
    pub wallpaper: Rc<ListView<Msg>>,
    pub anim: Rc<CheckBox<Msg>>,
    _mounted: Mounted<Msg>,
}

/// A swatch grid of `colors`, six to a row like the presets, raising `msg`
/// with the picked colour.
fn swatches(colors: &[u32], msg: fn(u32) -> Msg) -> Build<ColorPicker<Msg>, Msg> {
    let colors: Vec<Color> = colors.iter().copied().map(Color::hex).collect();
    color_picker(&colors)
        .columns(6)
        .on_select(move |color| msg(pack(color)))
}

impl AppearancePage {
    /// Lays the page out in the container `page`, listing the desktop
    /// `pictures` beside the background colours.
    pub fn build(ui: &Ui<Msg>, page: WidgetId, pictures: &[String]) -> Result<AppearancePage> {
        let (mode, accent, background) = (Handle::new(), Handle::new(), Handle::new());
        let (wallpaper, anim) = (Handle::new(), Handle::new());
        let rows = wallpaper_ops::rows(pictures);
        let rows: Vec<&str> = rows.iter().map(String::as_str).collect();
        let mounted = ui.mount_in(
            page,
            column().gap(12).children((
                group(
                    "Theme",
                    column().child(
                        radio_group(&["Dark", "Light"])
                            .on_select(Msg::Mode)
                            .bind(&mode)
                            .size(200, 56)
                            .align(Align::Start),
                    ),
                ),
                group(
                    "Accent color",
                    column().child(
                        swatches(&ACCENTS.map(|a| a.1), Msg::Accent)
                            .bind(&accent)
                            .size(SWATCHES.0, SWATCHES.1)
                            .align(Align::Start),
                    ),
                ),
                // A colour on the left, or a picture over it on the right.
                group(
                    "Desktop background",
                    row().gap(16).children((
                        swatches(&BACKGROUNDS.map(|b| b.1), Msg::Background)
                            .bind(&background)
                            .size(SWATCHES.0, SWATCHES.1)
                            .align(Align::Start),
                        choice_list(&rows)
                            .on_select(Msg::Wallpaper)
                            .bind(&wallpaper)
                            .fill(1)
                            .max_height(112),
                    )),
                ),
                group(
                    "Animations",
                    column().child(
                        checkbox("Window animations")
                            .on_toggle(Msg::Anim)
                            .bind(&anim),
                    ),
                ),
                row().gap(8).children((
                    button("Reset to defaults").on_click(Msg::ResetAppearance),
                    button("Make this the default for everyone").on_click(Msg::MakeDefault),
                )),
            )),
        )?;
        Ok(AppearancePage {
            mode: mode.get(),
            accent: accent.get(),
            background: background.get(),
            wallpaper: wallpaper.get(),
            anim: anim.get(),
            _mounted: mounted,
        })
    }
}
