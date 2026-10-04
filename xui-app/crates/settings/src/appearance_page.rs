//! The Appearance page: the theme, accent and wallpaper (colour or one of the
//! desktop pictures, [`wallpaper_ops`]) choices, each in a
//! card under an upper-case caption, on the window itself rather than on a
//! card of its own (docs/xui-theme-proposals.md, Midnight).

use xui_core::app::Ui;
use xui_core::backend::Result;
use xui_core::widget::{Button, CheckBox, ColorPicker, Label, ListView, Panel, RadioGroup};
use xui_core::{Color, Rect};

use crate::app::Msg;
use crate::theme_ops::{ACCENTS, BACKGROUNDS};
use crate::wallpaper_ops;

/// Width of the cards, inside the page's margins.
const CARD_W: i32 = 478;
/// Left edge of the captions and cards.
const LEFT: i32 = 16;

/// A design-pixel rectangle at the window's scale (docs/hidpi-plan.md).
fn rect(x: i32, y: i32, w: i32, h: i32) -> Rect {
    crate::layout::rect(x, y, w, h)
}

/// A `Color` as `0xRRGGBB`.
fn pack(color: Color) -> u32 {
    (u32::from(color.r) << 16) | (u32::from(color.g) << 8) | u32::from(color.b)
}

/// The page's widgets.
pub struct AppearancePage {
    page: Panel<Msg>,
    pub mode: RadioGroup<Msg>,
    pub accent: ColorPicker<Msg>,
    pub background: ColorPicker<Msg>,
    /// The desktop pictures: "None", then one row per picture.
    pub wallpaper: ListView<Msg>,
    pub anim: CheckBox<Msg>,
    // Owned only to keep their nodes registered.
    _captions: Vec<Label<Msg>>,
    _cards: Vec<Panel<Msg>>,
    _reset: Button<Msg>,
}

/// A caption at `top`, then a card of `height` under it; returns the card.
fn section(
    p: &Ui<Msg>,
    captions: &mut Vec<Label<Msg>>,
    top: i32,
    caption: &str,
    height: i32,
) -> Result<Panel<Msg>> {
    captions.push(Label::new(p, rect(LEFT + 2, top, 300, 18), caption)?.caption());
    Panel::new(p, rect(LEFT, top + 20, CARD_W, height))
}

/// A swatch row inside `card`, six to a row like the presets.
fn swatches(card: &Panel<Msg>, colors: impl Iterator<Item = u32>) -> Result<ColorPicker<Msg>> {
    let colors: Vec<Color> = colors.map(Color::hex).collect();
    Ok(ColorPicker::new(card.ui(), rect(6, 4, 300, 48), &colors)?.columns(6))
}

impl AppearancePage {
    /// Builds the page over `bounds`, hidden until shown, listing the
    /// desktop `pictures` beside the background colours.
    pub fn build(ui: &Ui<Msg>, bounds: Rect, pictures: &[String]) -> Result<AppearancePage> {
        let page = Panel::plain(ui, bounds)?;
        let p = page.ui();
        let mut captions = Vec::new();
        let mut cards = Vec::new();

        let theme = section(p, &mut captions, 0, "Theme", 64)?;
        let mode = RadioGroup::new(theme.ui(), rect(14, 6, 200, 52), &["Dark", "Light"])?
            .on_select(|i| Some(Msg::Mode(i)));
        cards.push(theme);

        let accent_card = section(p, &mut captions, 96, "Accent color", 56)?;
        let accent = swatches(&accent_card, ACCENTS.iter().map(|a| a.1))?
            .on_select(|c| Some(Msg::Accent(pack(c))));
        cards.push(accent_card);

        // A colour on the left, or a picture over it on the right.
        let background_card = section(p, &mut captions, 184, "Desktop background", 124)?;
        let background = swatches(&background_card, BACKGROUNDS.iter().map(|b| b.1))?
            .on_select(|c| Some(Msg::Background(pack(c))));
        let rows = wallpaper_ops::rows(pictures);
        let rows: Vec<&str> = rows.iter().map(String::as_str).collect();
        let wallpaper = ListView::new(background_card.ui(), rect(316, 6, 156, 112), &rows)?
            .multi_select(false)
            .on_select(|i| Some(Msg::Wallpaper(i)));
        cards.push(background_card);

        let anim_card = Panel::new(p, rect(LEFT, 340, CARD_W, 40))?;
        let anim = CheckBox::new(anim_card.ui(), rect(14, 8, 260, 24), "Window animations")?
            .on_toggle(|on| Some(Msg::Anim(on)));
        cards.push(anim_card);

        let reset = Button::new(p, rect(LEFT, 394, 170, 32), "Reset to defaults")?
            .on_click(|| Some(Msg::ResetAppearance));

        Ok(AppearancePage {
            page,
            mode,
            accent,
            background,
            wallpaper,
            anim,
            _captions: captions,
            _cards: cards,
            _reset: reset,
        })
    }

    /// Shows or hides the page.
    pub fn set_visible(&self, visible: bool) {
        self.page.set_visible(visible);
    }
}
