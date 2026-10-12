//! The tools that fill or read an area, through the model with real drags:
//! the Paint Bucket (tolerance, contiguity, sampling), the Color Picker and
//! the Gradient. No window, no `Ui`.

use xui_paint::model::tools::{FillArea, GradientKind, GradientOptions, Sample, SampleSize};
use xui_paint::model::{Bitmap, Model, Mods, Pixel, Side, Tool, WHITE};

const BLACK: Pixel = [0, 0, 0, 255];
const RED: Pixel = [255, 0, 0, 255];
const NONE: Mods = Mods::NONE;

fn drag(m: &mut Model, tool: Tool, a: (i32, i32), b: (i32, i32)) {
    m.set_tool(tool);
    m.begin_with(a.0, a.1, Side::Primary, NONE);
    m.extend_with(b.0, b.1, NONE);
    m.end();
}

fn px(m: &Model, x: i32, y: i32) -> Pixel {
    m.bitmap().get(x, y).expect("inside the layer")
}

fn inked(m: &Model) -> usize {
    let (w, h) = m.bitmap().size();
    (0..h as i32)
        .flat_map(|y| (0..w as i32).map(move |x| (x, y)))
        .filter(|&(x, y)| px(m, x, y) != WHITE)
        .count()
}

fn model() -> Model {
    Model::new(96, 96)
}

#[test]
fn the_bucket_honours_tolerance_contiguity_and_the_sampled_layer() {
    let mut bitmap = Bitmap::white(40, 40);
    for y in 0..40 {
        // A vertical wall, and a slightly off-white band on the right.
        bitmap.put(20, y, BLACK);
        for x in 21..40 {
            bitmap.put(x, y, [240, 240, 240, 255]);
        }
    }
    // An off-white island on the left, the same colour as the right side.
    bitmap.put(3, 3, [240, 240, 240, 255]);
    let mut m = Model::new(2, 2);
    m.load(bitmap).unwrap();
    m.set_primary(RED);
    m.set_tool(Tool::Fill);
    // Tolerance 0: only the exactly equal white to the left of the wall.
    m.begin(10, 10, Side::Primary);
    assert_eq!(px(&m, 0, 0), RED);
    assert_eq!(
        px(&m, 30, 5),
        [240, 240, 240, 255],
        "the other side is intact"
    );
    assert_eq!(
        px(&m, 3, 3),
        [240, 240, 240, 255],
        "and the island, not equal"
    );
    m.undo();
    // 10 % takes the island too (contiguous: the island touches white).
    let mut fill = m.fill();
    fill.tolerance = 10;
    m.set_fill(fill);
    m.begin(10, 10, Side::Primary);
    assert_eq!(px(&m, 3, 3), RED, "within tolerance");
    assert_eq!(
        px(&m, 30, 5),
        [240, 240, 240, 255],
        "the wall still bounds it"
    );
    m.undo();
    // Global reaches across the wall.
    fill.area = FillArea::Global;
    m.set_fill(fill);
    m.begin(10, 10, Side::Primary);
    assert_eq!(px(&m, 30, 5), RED);
    assert_eq!(px(&m, 20, 5), BLACK, "but not the wall");
    assert_eq!(m.history().undo_len(), 1, "one step per click");
}

#[test]
fn the_bucket_can_compare_the_whole_image_instead_of_the_layer() {
    let mut m = model();
    for y in 0..96 {
        // A wall on the background layer.
        m.set_tool(Tool::Pencil);
        m.begin(48, y, Side::Primary);
        m.end();
    }
    m.add_layer().unwrap();
    m.set_primary(RED);
    m.set_tool(Tool::Fill);
    m.begin(10, 10, Side::Primary);
    assert_eq!(px(&m, 80, 10), RED, "the empty layer fills end to end");
    m.undo();
    let mut fill = m.fill();
    fill.sample = Sample::Image;
    m.set_fill(fill);
    m.begin(10, 10, Side::Primary);
    assert_eq!(px(&m, 10, 10), RED);
    assert_eq!(px(&m, 80, 10), [0, 0, 0, 0], "the wall below stops it");
}

#[test]
fn the_picker_averages_samples_the_image_and_can_switch_back() {
    let mut m = Model::new(2, 2);
    let mut bitmap = Bitmap::white(8, 8);
    bitmap.put(3, 3, BLACK);
    m.load(bitmap).unwrap();
    m.set_tool(Tool::Brush);
    m.set_tool(Tool::Picker);
    m.begin(3, 3, Side::Primary);
    assert_eq!(m.primary(), BLACK, "one pixel");
    assert_eq!(m.tool(), Tool::Picker, "it stays unless asked");
    let mut picker = m.picker();
    picker.size = SampleSize::Three;
    m.set_picker(picker);
    m.begin(3, 3, Side::Secondary);
    let grey = m.secondary();
    assert!(grey[0] > 200 && grey[0] < 255, "3x3 average: {grey:?}");
    picker.switch_back = true;
    picker.size = SampleSize::One;
    m.set_picker(picker);
    m.begin(0, 0, Side::Primary);
    assert_eq!(m.primary(), WHITE);
    assert_eq!(m.tool(), Tool::Brush, "back to the tool before the picker");
}

#[test]
fn the_picker_reads_the_image_when_asked() {
    let mut m = model();
    m.add_layer().unwrap();
    m.set_tool(Tool::Picker);
    m.begin(5, 5, Side::Primary);
    assert_eq!(m.primary(), [0, 0, 0, 0], "the layer is empty there");
    let mut picker = m.picker();
    picker.sample = Sample::Image;
    m.set_picker(picker);
    m.begin(5, 5, Side::Primary);
    assert_eq!(m.primary(), WHITE, "the image shows the background");
}

fn gradient(kind: GradientKind) -> Model {
    let mut m = model();
    m.set_primary(BLACK);
    m.set_secondary(WHITE);
    m.set_gradient(GradientOptions { kind });
    m
}

#[test]
fn a_linear_gradient_runs_from_primary_to_secondary_along_the_drag() {
    let mut m = gradient(GradientKind::Linear);
    drag(&mut m, Tool::Gradient, (10, 48), (86, 48));
    assert!(px(&m, 10, 48)[0] < 8, "primary at the start");
    assert!(px(&m, 86, 48)[0] > 247, "secondary at the end");
    let middle = px(&m, 48, 48)[0];
    assert!((100..160).contains(&middle), "{middle}");
    assert!(px(&m, 30, 48)[0] < px(&m, 60, 48)[0], "monotonic");
    assert_eq!(px(&m, 30, 10), px(&m, 30, 80), "constant across the drag");
    assert_eq!(m.history().undo_len(), 1);
    // The right button runs it the other way.
    let mut r = gradient(GradientKind::Linear);
    r.set_tool(Tool::Gradient);
    r.begin(10, 48, Side::Secondary);
    r.extend(86, 48);
    r.end();
    assert!(px(&r, 10, 48)[0] > 247 && px(&r, 86, 48)[0] < 8);
}

#[test]
fn reflected_radial_and_diamond_gradients_have_their_shapes() {
    let mut reflected = gradient(GradientKind::LinearReflected);
    drag(&mut reflected, Tool::Gradient, (48, 48), (80, 48));
    assert_eq!(px(&reflected, 48 - 10, 48), px(&reflected, 48 + 10, 48));
    let mut radial = gradient(GradientKind::Radial);
    drag(&mut radial, Tool::Gradient, (48, 48), (80, 48));
    assert_eq!(px(&radial, 48, 48 + 16), px(&radial, 48 + 16, 48));
    assert!(px(&radial, 48, 48)[0] < 8);
    let mut diamond = gradient(GradientKind::Diamond);
    drag(&mut diamond, Tool::Gradient, (48, 48), (80, 48));
    // On the diagonal a diamond reaches the secondary sooner than a circle.
    assert!(px(&diamond, 48 + 20, 48 + 20)[0] > px(&radial, 48 + 20, 48 + 20)[0]);
}

#[test]
fn a_gradient_fills_the_selection_only_and_a_click_paints_nothing() {
    let mut m = gradient(GradientKind::Linear);
    drag(&mut m, Tool::RectSelect, (30, 30), (60, 60));
    drag(&mut m, Tool::Gradient, (0, 48), (95, 48));
    assert_ne!(px(&m, 45, 45), WHITE, "inside the selection");
    assert_eq!(px(&m, 10, 45), WHITE, "outside it");
    assert_eq!(px(&m, 45, 10), WHITE);
    let steps = m.history().undo_len();
    // A drag of no length has no direction.
    m.set_tool(Tool::Gradient);
    m.begin(40, 40, Side::Primary);
    m.end();
    assert_eq!(m.history().undo_len(), steps, "no history for nothing");
}

#[test]
fn the_gradient_preview_follows_the_pointer_and_leaves_nothing_behind() {
    let mut m = gradient(GradientKind::Linear);
    m.set_tool(Tool::Gradient);
    m.begin(10, 48, Side::Primary);
    m.extend(86, 48);
    let wide = px(&m, 48, 48);
    assert_eq!(m.history().undo_len(), 0, "a preview is not history");
    m.extend(30, 48);
    assert_ne!(px(&m, 48, 48), wide, "the preview moved with the pointer");
    assert!(px(&m, 80, 48)[0] > 247, "past the end it is the secondary");
    m.cancel();
    assert_eq!(inked(&m), 0, "Esc leaves the layer as it was");
    assert_eq!(m.history().undo_len(), 0);
}
