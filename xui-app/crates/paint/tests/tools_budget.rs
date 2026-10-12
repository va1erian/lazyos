//! Live work (a float, a gradient, a shape, a bucket fill) counts the memory it
//! holds against the budget and is refused, with a notice, when it cannot fit.

use xui_paint::model::tools::{FillArea, FillOptions, Sample};
use xui_paint::model::{Model, Side, Tool, WHITE};

/// A layer of 8192 x 4096: 128 MiB, with its cache and the undo reserve
/// 320 MiB of the 512 MiB budget.
fn big() -> Model {
    Model::new(8192, 4096)
}

#[test]
fn a_gradient_over_a_huge_layer_is_refused_before_it_backs_the_layer_up() {
    let mut m = big();
    m.set_tool(Tool::Gradient);
    m.begin(0, 0, Side::Primary);
    assert!(!m.is_dragging());
    assert!(m.take_notice().is_some_and(|n| n.contains("memory")));
    // Inside a small selection the same gradient is fine.
    m.set_tool(Tool::RectSelect);
    m.begin(0, 0, Side::Primary);
    m.extend(99, 99);
    m.end();
    m.set_tool(Tool::Gradient);
    m.begin(0, 0, Side::Primary);
    assert!(m.is_dragging());
    m.extend(99, 99);
    m.end();
}

#[test]
fn lifting_a_huge_layer_is_refused_but_a_small_selection_lifts() {
    let mut m = big();
    m.set_tool(Tool::MovePixels);
    m.begin(10, 10, Side::Primary);
    assert!(!m.is_floating());
    assert!(m.take_notice().is_some());
    m.set_tool(Tool::RectSelect);
    m.begin(0, 0, Side::Primary);
    m.extend(63, 63);
    m.end();
    m.set_tool(Tool::MovePixels);
    m.begin(10, 10, Side::Primary);
    assert!(m.is_floating());
}

#[test]
fn a_shape_stops_growing_when_its_capture_would_not_fit() {
    let mut m = big();
    m.set_tool(Tool::Rectangle);
    m.begin(0, 0, Side::Primary);
    m.extend(8000, 4000);
    assert!(m.take_notice().is_some());
    assert_eq!(m.bitmap().get(4000, 0), Some(WHITE), "nothing was drawn");
    m.extend(100, 100);
    m.end();
}

#[test]
fn a_bucket_fill_of_a_huge_region_is_refused() {
    let mut m = big();
    m.set_tool(Tool::Fill);
    m.set_fill(FillOptions {
        area: FillArea::Contiguous,
        sample: Sample::Image,
        ..FillOptions::default()
    });
    m.set_primary([255, 0, 0, 255]);
    m.begin(5, 5, Side::Primary);
    assert!(m.take_notice().is_some());
    assert_eq!(m.bitmap().get(5, 5), Some(WHITE));
    assert_eq!(m.history().undo_len(), 0);
}
