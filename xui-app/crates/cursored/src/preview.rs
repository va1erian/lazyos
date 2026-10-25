//! The preview strip: the pointer as `xuid` will draw it, from the same
//! rasteriser, on a light and a dark background at the real sizes.

use std::cell::RefCell;
use std::rc::Rc;

use cursorgfx::{render_themed, Shape, Sprite, Theme, SIZES};
use xui_core::app::Ui;
use xui_core::backend::{Canvas, NodeKind, NodeSpec, Result, WidgetId};
use xui_core::geometry::{Rect, Size};
use xui_core::image::Image;
use xui_core::layout::Constraints;
use xui_core::widget::{Control, Placeable};
use xui_core::Color;

use crate::model::Model;
use crate::Msg;

/// Design size of the tiles that show every shape.
const TILE: u32 = 16;
const GAP: i32 = 6;

/// A sprite (premultiplied `0xAARRGGBB`) as a straight-alpha RGBA image.
pub fn premultiplied_to_image(sprite: &Sprite) -> Option<Image> {
    let mut bytes = Vec::with_capacity(sprite.pixels.len() * 4);
    for &p in &sprite.pixels {
        let a = p >> 24;
        let un = |shift: u32| {
            let c = (p >> shift) & 0xff;
            (c * 255).checked_div(a).map_or(0, |v| v.min(255) as u8)
        };
        bytes.extend_from_slice(&[un(16), un(8), un(0), a as u8]);
    }
    Image::from_rgba(sprite.w, sprite.h, bytes).ok()
}

struct Cache {
    theme: Theme,
    shape: Shape,
    /// The current shape at each design size, then every shape at `TILE`.
    sizes: Vec<Image>,
    shapes: Vec<Image>,
}

pub struct Preview {
    model: Rc<RefCell<Model>>,
    cache: RefCell<Option<Cache>>,
}

impl Preview {
    pub fn new(model: Rc<RefCell<Model>>) -> Preview {
        Preview {
            model,
            cache: RefCell::new(None),
        }
    }

    fn images(theme: &Theme, px: impl Iterator<Item = (Shape, u32)>) -> Vec<Image> {
        px.filter_map(|(s, size)| premultiplied_to_image(&render_themed(theme, s, size, 0)))
            .collect()
    }
}

fn strip(canvas: &mut dyn Canvas, bounds: Rect, top: i32, images: &[Image], bg: u32) -> i32 {
    let height = images.iter().map(|i| i.height() as i32).max().unwrap_or(0) + 2 * GAP;
    canvas.fill_rect(
        Rect::new(bounds.left, top, bounds.right, top + height),
        Color::hex(bg),
    );
    let mut x = bounds.left + GAP;
    for image in images {
        let (w, h) = (image.width() as i32, image.height() as i32);
        let y = top + (height - h) / 2;
        canvas.draw_image(image, Rect::new(x, y, x + w, y + h));
        x += w + GAP;
    }
    top + height
}

impl Preview {
    pub fn paint(&self, canvas: &mut dyn Canvas) {
        let bounds = canvas.bounds();
        let Ok(model) = self.model.try_borrow() else {
            return;
        };
        let stale = self
            .cache
            .borrow()
            .as_ref()
            .is_none_or(|c| c.theme != model.theme || c.shape != model.shape);
        if stale {
            let theme = model.theme.clone();
            let sizes = Preview::images(&theme, SIZES.iter().map(|&s| (model.shape, s)));
            let shapes = Preview::images(&theme, Shape::ALL.iter().map(|&s| (s, TILE)));
            *self.cache.borrow_mut() = Some(Cache {
                theme,
                shape: model.shape,
                sizes,
                shapes,
            });
        }
        let cache = self.cache.borrow();
        let Some(cache) = cache.as_ref() else { return };
        let mut y = bounds.top;
        for (images, bg) in [
            (&cache.sizes, 0xf2f2f2),
            (&cache.sizes, 0x202020),
            (&cache.shapes, 0xf2f2f2),
            (&cache.shapes, 0x202020),
        ] {
            y = strip(canvas, bounds, y, images, bg);
        }
    }
}

/// The control that hosts the preview.
pub struct PreviewControl {
    control: Control<Msg>,
    ui: Ui<Msg>,
}

impl PreviewControl {
    pub fn new(ui: &Ui<Msg>, preview: Rc<Preview>) -> Result<PreviewControl> {
        let control = Control::new(ui, &NodeSpec::new(NodeKind::Custom, Rect::default()))?;
        control.set_painter(Rc::new(move |canvas| preview.paint(canvas)));
        Ok(PreviewControl {
            control,
            ui: ui.clone(),
        })
    }

    pub fn invalidate(&self) {
        self.ui.invalidate(self.control.id());
    }
}

impl Placeable<Msg> for PreviewControl {
    fn id(&self) -> WidgetId {
        self.control.id()
    }

    fn measure(&self, _ui: &Ui<Msg>, _constraints: Constraints) -> Size {
        // Four strips of the tallest sprite (the 48 px one).
        Size::new(0, 4 * (48 + 2 * 4 + 2 * GAP))
    }

    fn placed(&self, _ui: &Ui<Msg>, _rect: Rect) {}
}
