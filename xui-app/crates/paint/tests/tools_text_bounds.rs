//! The Text tool never builds glyph coverage bigger than the layer it paints
//! on, however big the text is, and refuses what the memory budget cannot hold.

use std::cell::Cell;
use std::rc::Rc;

use xui_paint::model::text::{LineGeometry, TextCoverage, TextRasterizer, TextStyle};
use xui_paint::model::tools::Coverage;
use xui_paint::model::{Model, Pixel, Rect, Side, Tool, WHITE};

const BLACK: Pixel = [0, 0, 0, 255];

/// A block `width` x 10 pixels, all ink, that allocates only the part of it
/// inside the window it is asked for, and notes the largest it allocated.
struct Wide {
    width: u32,
    height: u32,
    allocated: Rc<Cell<usize>>,
}

impl TextRasterizer for Wide {
    fn layout_in(&self, _text: &str, _style: &TextStyle, window: Rect) -> TextCoverage {
        let extent = Rect::new(0, 0, self.width, self.height);
        let keep = extent.intersect(window);
        let mut mask = Coverage::new(keep);
        mask.data_mut().fill(255);
        self.allocated
            .set(self.allocated.get().max(mask.data().len()));
        TextCoverage {
            extent,
            mask,
            lines: vec![LineGeometry {
                top: 0.0,
                height: 10.0,
                baseline: 8.0,
                start: 0,
                carets: vec![0.0, 6.0],
            }],
        }
    }
}

fn model(width: u32, height: u32, layer: (u32, u32)) -> (Model, Rc<Cell<usize>>) {
    let allocated = Rc::new(Cell::new(0));
    let mut m = Model::new(layer.0, layer.1);
    m.set_text_rasterizer(Rc::new(Wide {
        width,
        height,
        allocated: allocated.clone(),
    }));
    m.set_tool(Tool::Text);
    m.begin(5, 5, Side::Primary);
    m.end();
    (m, allocated)
}

#[test]
fn the_mask_is_clipped_to_the_layer_not_the_text() {
    // A block 100 million pixels wide on a 120 x 120 layer.
    let (mut m, allocated) = model(100_000_000, 10, (120, 120));
    m.type_char('W');
    assert!(allocated.get() <= 120 * 120, "{}", allocated.get());
    assert_eq!(
        m.bitmap().get(100, 8),
        Some(BLACK),
        "the visible part is drawn"
    );
    assert_eq!(m.bitmap().get(100, 30), Some(WHITE));
    let block = m.text_block().expect("the block spans the whole text");
    assert_eq!(block.width(), 100_000_000);
    m.type_char('W');
    assert!(allocated.get() <= 120 * 120, "and so is every later key");
}

#[test]
fn dragging_the_handle_across_the_whole_range_does_not_overflow() {
    let (mut m, _) = model(10, 10, (120, 120));
    m.type_char('W');
    let handle = m.text_handle().expect("a handle");
    m.begin(handle.x0, handle.y0, Side::Primary);
    m.extend(i32::MIN, i32::MIN);
    m.extend(i32::MAX, i32::MAX);
    m.extend(i32::MIN, i32::MAX);
    m.end();
    assert!(m.is_editing_text());
}

#[test]
fn a_block_the_budget_cannot_hold_is_refused_with_a_notice() {
    // 8192 x 4096 layer: its coverage would need 10 bytes a pixel on top of
    // the layer, its cache and the undo reserve.
    let (mut m, _) = model(8192, 4096, (8192, 4096));
    m.type_char('W');
    let notice = m.take_notice().expect("a clear status message");
    assert!(notice.contains("too large"), "{notice}");
    assert_eq!(m.bitmap().get(100, 8), Some(WHITE), "nothing was painted");
    // A block that fits the same layer is painted.
    let (mut small, _) = model(40, 10, (8192, 4096));
    small.type_char('W');
    assert_eq!(small.bitmap().get(20, 8), Some(BLACK));
}
