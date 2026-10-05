//! The Appearance page: the theme, accent and wallpaper (colour or one of the
//! desktop pictures, [`wallpaper_ops`]) choices, each a titled section, on the
//! window itself rather than on a card of its own
//! (docs/xui-theme-proposals.md, Midnight).

use std::rc::Rc;

use xui_core::app::Ui;
use xui_core::arrange::{
    build, button, checkbox, column, group, row, Build, Handle, LayoutExt, Mounted,
};
use xui_core::backend::{Result, WidgetId};
use xui_core::layout::Align;
use xui_core::widget::{CheckBox, ColorPicker, ListView, RadioGroup};
use xui_core::{Color, Rect};

use crate::app::Msg;
use crate::place::{placed, Placed};
use crate::theme_ops::{ACCENTS, BACKGROUNDS};
use crate::wallpaper_ops;

/// A `Color` as `0xRRGGBB`.
pub fn pack(color: Color) -> u32 {
    (u32::from(color.r) << 16) | (u32::from(color.g) << 8) | u32::from(color.b)
}

/// The page's widgets.
pub struct AppearancePage {
    pub mode: Rc<Placed<RadioGroup<Msg>>>,
    pub accent: Rc<Placed<ColorPicker<Msg>>>,
    pub background: Rc<Placed<ColorPicker<Msg>>>,
    /// The desktop pictures: "None", then one row per picture.
    pub wallpaper: Rc<ListView<Msg>>,
    pub anim: Rc<CheckBox<Msg>>,
    _mounted: Mounted<Msg>,
}

/// A swatch grid of `colors`, six to a row like the presets, raising `msg`
/// with the picked colour.
fn swatches(colors: Vec<u32>, msg: fn(u32) -> Msg) -> Build<Placed<ColorPicker<Msg>>, Msg> {
    placed(300, 48, move |ui, bounds| {
        let colors: Vec<Color> = colors.into_iter().map(Color::hex).collect();
        Ok(ColorPicker::new(ui, bounds, &colors)?
            .columns(6)
            .on_select(move |color| Some(msg(pack(color)))))
    })
}

impl AppearancePage {
    /// Lays the page out in the container `page`, listing the desktop
    /// `pictures` beside the background colours.
    pub fn build(ui: &Ui<Msg>, page: WidgetId, pictures: &[String]) -> Result<AppearancePage> {
        let (mode, accent, background) = (Handle::new(), Handle::new(), Handle::new());
        let (wallpaper, anim) = (Handle::new(), Handle::new());
        let rows = wallpaper_ops::rows(pictures);
        let mounted = ui.mount_in(
            page,
            column().gap(12).children((
                group(
                    "Theme",
                    column().child(
                        placed(200, 56, |ui, bounds| {
                            Ok(RadioGroup::new(ui, bounds, &["Dark", "Light"])?
                                .on_select(|i| Some(Msg::Mode(i))))
                        })
                        .bind(&mode)
                        .align(Align::Start),
                    ),
                ),
                group(
                    "Accent color",
                    column().child(
                        swatches(ACCENTS.iter().map(|a| a.1).collect(), Msg::Accent)
                            .bind(&accent)
                            .align(Align::Start),
                    ),
                ),
                // A colour on the left, or a picture over it on the right.
                group(
                    "Desktop background",
                    row().gap(16).children((
                        swatches(BACKGROUNDS.iter().map(|b| b.1).collect(), Msg::Background)
                            .bind(&background)
                            .align(Align::Start),
                        build(move |ui| {
                            let rows: Vec<&str> = rows.iter().map(String::as_str).collect();
                            Ok(ListView::new(ui, Rect::default(), &rows)?
                                .multi_select(false)
                                .on_select(|i| Some(Msg::Wallpaper(i))))
                        })
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
                button("Reset to defaults")
                    .on_click(Msg::ResetAppearance)
                    .align(Align::Start),
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
