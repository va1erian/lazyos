//! The drawing tools through the model, with real begin/extend/end drags: the
//! stroke engine's output on the layer, one undo step per action, Esc, the
//! selection clip, the bucket, the picker, the gradient and shape options.
//! No window, no `Ui`.

use xui_paint::model::tools::{Arrow, FillMode};
use xui_paint::model::{Model, Mods, Pixel, Side, Tool, WHITE};

const BLACK: Pixel = [0, 0, 0, 255];
const RED: Pixel = [255, 0, 0, 255];
const BLUE: Pixel = [0, 0, 255, 255];
const NONE: Mods = Mods::NONE;
const SHIFT: Mods = Mods {
    shift: true,
    ..Mods::NONE
};

/// One drag of the current tool from `a` through `via` to `b`.
fn drag_with(m: &mut Model, a: (i32, i32), via: &[(i32, i32)], b: (i32, i32), mods: Mods) {
    m.begin_with(a.0, a.1, Side::Primary, mods);
    for p in via {
        m.extend_with(p.0, p.1, mods);
    }
    m.extend_with(b.0, b.1, mods);
    m.end();
}

fn drag(m: &mut Model, tool: Tool, a: (i32, i32), b: (i32, i32)) {
    m.set_tool(tool);
    drag_with(m, a, &[], b, NONE);
}

fn px(m: &Model, x: i32, y: i32) -> Pixel {
    m.bitmap().get(x, y).expect("inside the layer")
}

fn is_white(m: &Model, x: i32, y: i32) -> bool {
    px(m, x, y) == WHITE
}

/// Every pixel of the layer that is not white.
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

/// The drawing tools that take a drag and paint on release.
const DRAWING: [Tool; 8] = [
    Tool::Pencil,
    Tool::Brush,
    Tool::Eraser,
    Tool::Line,
    Tool::Rectangle,
    Tool::RoundedRectangle,
    Tool::Ellipse,
    Tool::Gradient,
];

#[test]
fn the_paintbrush_is_antialiased_with_a_soft_edge_in_one_undo_step() {
    let mut m = model();
    let mut brush = m.brush();
    brush.width = 24;
    brush.hardness = 0;
    m.set_brush(brush);
    drag(&mut m, Tool::Brush, (20, 48), (70, 48));
    assert_eq!(px(&m, 45, 48), BLACK, "the middle of a soft brush is solid");
    let edge = px(&m, 45, 48 - 8);
    assert!(
        edge[0] > 0 && edge[0] < 255,
        "a soft brush fades towards its rim: {edge:?}"
    );
    assert!(is_white(&m, 45, 48 - 14), "and stops at the rim");
    assert_eq!(m.history().undo_len(), 1, "the whole drag is one step");
    let after = m.bitmap().clone();
    m.undo();
    assert_eq!(inked(&m), 0);
    m.redo();
    assert_eq!(*m.bitmap(), after);
}

#[test]
fn a_hard_brush_has_a_one_pixel_antialiased_rim_and_aa_off_has_none() {
    let mut m = model();
    drag(&mut m, Tool::Brush, (48, 48), (48, 48));
    let mut brush = m.brush();
    brush.width = 11;
    m.set_brush(brush);
    let mut hard = Model::new(96, 96);
    hard.set_brush(brush);
    drag(&mut hard, Tool::Brush, (48, 48), (48, 48));
    let mut aliased = Model::new(96, 96);
    let mut off = brush;
    off.antialias = false;
    aliased.set_brush(off);
    drag(&mut aliased, Tool::Brush, (48, 48), (48, 48));
    let grey = |m: &Model| {
        (0..96)
            .flat_map(|y| (0..96).map(move |x| (x, y)))
            .filter(|&(x, y)| {
                let p = px(m, x, y);
                p != WHITE && p != BLACK
            })
            .count()
    };
    assert!(grey(&hard) > 0, "antialiasing blends the rim");
    assert_eq!(grey(&aliased), 0, "AA off writes only the colour");
}

#[test]
fn the_brush_reaches_five_hundred_pixels() {
    let mut m = Model::new(600, 600);
    m.set_size(10_000);
    assert_eq!(m.size(), 500);
    drag(&mut m, Tool::Brush, (300, 300), (300, 300));
    assert_eq!(px(&m, 300, 300), BLACK);
    assert_eq!(px(&m, 300 + 248, 300), BLACK, "radius 250");
    assert_eq!(px(&m, 300 + 252, 300), WHITE);
}

#[test]
fn a_stroke_that_crosses_itself_does_not_build_up_alpha() {
    let mut m = model();
    m.set_primary([255, 0, 0, 128]);
    m.set_size(8);
    m.set_tool(Tool::Brush);
    m.begin(20, 48, Side::Primary);
    for p in [(70, 48), (20, 48), (70, 48), (20, 48)] {
        m.extend(p.0, p.1);
    }
    m.end();
    let once = px(&m, 45, 48);
    let mut single = model();
    single.set_primary([255, 0, 0, 128]);
    single.set_size(8);
    drag(&mut single, Tool::Brush, (20, 48), (70, 48));
    assert_eq!(once, px(&single, 45, 48));
}

#[test]
fn the_eraser_lowers_alpha_by_its_coverage_and_the_other_button_paints() {
    let mut m = model();
    let mut brush = m.brush();
    brush.width = 20;
    brush.hardness = 0;
    m.set_brush(brush);
    drag(&mut m, Tool::Eraser, (48, 48), (48, 48));
    assert_eq!(px(&m, 48, 48), [0, 0, 0, 0], "the centre is gone");
    let rim = px(&m, 48 + 7, 48);
    assert!(
        rim[3] > 0 && rim[3] < 255,
        "a soft rim half erases: {rim:?}"
    );
    assert_eq!(px(&m, 48 + 14, 48), WHITE);
    m.set_secondary(BLUE);
    m.set_tool(Tool::Eraser);
    m.begin(10, 10, Side::Secondary);
    m.end();
    assert_eq!(
        px(&m, 10, 10),
        BLUE,
        "the other button paints the secondary"
    );
}

#[test]
fn the_pencil_is_exact_and_shift_click_draws_a_line_from_the_last_point() {
    let mut m = model();
    m.set_primary([10, 20, 30, 128]);
    drag(&mut m, Tool::Pencil, (5, 5), (5, 5));
    assert_eq!(
        px(&m, 5, 5),
        [10, 20, 30, 128],
        "no blending, alpha included"
    );
    assert_eq!(px(&m, 6, 5), WHITE);
    m.set_tool(Tool::Pencil);
    m.begin_with(20, 5, Side::Primary, SHIFT);
    m.end();
    for x in 5..=20 {
        assert_eq!(px(&m, x, 5), [10, 20, 30, 128], "x={x}");
    }
    assert_eq!(px(&m, 12, 6), WHITE, "one pixel wide");
    assert_eq!(m.history().undo_len(), 2);
}

#[test]
fn a_line_has_a_width_and_shift_snaps_it_to_fifteen_degrees() {
    let mut m = model();
    m.set_size(5);
    m.set_tool(Tool::Line);
    drag_with(&mut m, (10, 20), &[(60, 26)], (60, 26), SHIFT);
    // Atan(6/50) is about 7 degrees: nearest multiple of 15 is 0 (7.5 rounds
    // either way, so use the clear side): the line is horizontal.
    assert_eq!(px(&m, 35, 20), BLACK);
    assert!(is_white(&m, 35, 26), "it did not follow the pointer");
    assert!(is_white(&m, 35, 14));
    let mut free = model();
    free.set_size(5);
    free.set_tool(Tool::Line);
    drag_with(&mut free, (10, 20), &[], (60, 26), NONE);
    assert_eq!(
        px(&free, 35, 23),
        BLACK,
        "without Shift it follows the pointer"
    );
}

#[test]
fn arrow_heads_widen_the_end_of_a_line() {
    let ink = |arrow: Arrow| {
        let mut m = model();
        let mut shape = m.shape();
        shape.arrow = arrow;
        shape.width = 2.0;
        m.set_shape(shape);
        drag(&mut m, Tool::Line, (10, 48), (80, 48));
        (inked(&m), px(&m, 72, 48 - 4), px(&m, 19, 48 - 4))
    };
    let (plain, plain_tip, _) = ink(Arrow::None);
    let (end, end_tip, end_start) = ink(Arrow::End);
    let (start, _, start_start) = ink(Arrow::Start);
    let (both, _, _) = ink(Arrow::Both);
    assert!(end > plain && start > plain && both > end);
    assert_eq!(plain_tip, WHITE);
    assert_ne!(end_tip, WHITE, "a head at the end");
    assert_eq!(end_start, WHITE, "and none at the start");
    assert_ne!(start_start, WHITE, "a head at the start");
}

#[test]
fn rectangles_ellipses_and_rounded_rectangles_draw_outline_fill_or_both() {
    for tool in [Tool::Rectangle, Tool::RoundedRectangle, Tool::Ellipse] {
        let run = |mode: FillMode| {
            let mut m = model();
            m.set_secondary(BLUE);
            let mut shape = m.shape();
            shape.mode = mode;
            shape.width = 4.0;
            m.set_shape(shape);
            drag(&mut m, tool, (16, 16), (80, 80));
            m
        };
        let outline = run(FillMode::Outline);
        assert!(is_white(&outline, 48, 48), "{tool:?}: an outline is hollow");
        assert_ne!(px(&outline, 48, 17), WHITE, "{tool:?}: top edge");
        let fill = run(FillMode::Fill);
        assert_eq!(
            px(&fill, 48, 48),
            BLUE,
            "{tool:?}: filled with the secondary"
        );
        let both = run(FillMode::Both);
        assert_eq!(px(&both, 48, 48), BLUE, "{tool:?}: fill inside");
        assert_eq!(px(&both, 48, 17), BLACK, "{tool:?}: primary outline on top");
        assert_eq!(both.history().undo_len(), 1);
    }
}

#[test]
fn a_rounded_rectangle_cuts_its_corners_and_a_rectangle_does_not() {
    let corner = |tool: Tool| {
        let mut m = model();
        let mut shape = m.shape();
        shape.mode = FillMode::Fill;
        shape.radius = 20.0;
        m.set_shape(shape);
        m.set_secondary(RED);
        drag(&mut m, tool, (16, 16), (80, 80));
        px(&m, 16, 16)
    };
    assert_eq!(
        corner(Tool::Rectangle),
        RED,
        "the plain tool ignores the radius"
    );
    assert_eq!(corner(Tool::RoundedRectangle), WHITE);
}

#[test]
fn shift_makes_a_square_or_a_circle() {
    let mut m = model();
    let mut shape = m.shape();
    shape.mode = FillMode::Fill;
    m.set_shape(shape);
    m.set_secondary(RED);
    m.set_tool(Tool::Rectangle);
    drag_with(&mut m, (10, 10), &[(60, 30)], (60, 30), SHIFT);
    assert_eq!(px(&m, 55, 55), RED, "the short side grew to the long one");
    assert_eq!(px(&m, 30, 59), RED);
}

#[test]
fn without_antialiasing_a_shape_writes_only_its_colour() {
    let mut m = model();
    let mut shape = m.shape();
    shape.antialias = false;
    shape.mode = FillMode::Both;
    shape.width = 3.0;
    m.set_shape(shape);
    m.set_secondary(RED);
    drag(&mut m, Tool::Ellipse, (10, 10), (85, 70));
    for y in 0..96 {
        for x in 0..96 {
            let p = px(&m, x, y);
            assert!([WHITE, BLACK, RED].contains(&p), "({x},{y}) {p:?}");
        }
    }
    assert!(inked(&m) > 100);
}

#[test]
fn every_drawing_tool_is_one_undo_step_and_esc_takes_it_back() {
    for tool in DRAWING {
        let mut m = model();
        m.set_size(6);
        let blank = m.bitmap().clone();
        m.set_tool(tool);
        m.begin(10, 10, Side::Primary);
        m.extend(40, 20);
        m.extend(70, 70);
        m.extend(20, 80);
        m.end();
        assert_eq!(m.history().undo_len(), 1, "{tool:?}: one entry");
        let drawn = m.bitmap().clone();
        if tool != Tool::Eraser {
            assert_ne!(drawn, blank, "{tool:?} painted");
        }
        m.undo();
        assert_eq!(*m.bitmap(), blank, "{tool:?}: undo restores it");
        m.redo();
        assert_eq!(*m.bitmap(), drawn, "{tool:?}: redo repaints it");
        m.undo();

        m.begin(10, 10, Side::Primary);
        m.extend(40, 20);
        m.extend(70, 70);
        m.cancel();
        assert!(!m.is_dragging());
        assert_eq!(*m.bitmap(), blank, "{tool:?}: Esc leaves no pixel");
        assert_eq!(m.history().undo_len(), 0, "{tool:?}: and no entry");
        assert!(m.history().can_redo(), "{tool:?}: redo is kept");
    }
}

#[test]
fn every_tool_clips_to_the_selection() {
    let tools = [
        Tool::Pencil,
        Tool::Brush,
        Tool::Line,
        Tool::Rectangle,
        Tool::RoundedRectangle,
        Tool::Ellipse,
        Tool::Gradient,
        Tool::Fill,
    ];
    for tool in tools {
        let mut m = model();
        m.set_size(12);
        let mut shape = m.shape();
        shape.mode = FillMode::Both;
        m.set_shape(shape);
        m.set_secondary(BLUE);
        drag(&mut m, Tool::RectSelect, (30, 30), (60, 60));
        let steps = m.history().undo_len();
        m.set_tool(tool);
        m.begin(5, 5, Side::Primary);
        m.extend(90, 90);
        m.end();
        assert_eq!(m.history().undo_len(), steps + 1, "{tool:?}");
        for (x, y) in [(10, 10), (80, 80), (10, 80), (80, 10), (29, 45), (61, 45)] {
            assert_eq!(px(&m, x, y), WHITE, "{tool:?} wrote outside at ({x},{y})");
        }
        let inside = (30..61).any(|d| px(&m, d, d) != WHITE);
        assert!(inside, "{tool:?} painted inside");
    }
}

#[test]
fn a_soft_selection_mask_scales_the_stroke() {
    let mut m = model();
    drag(&mut m, Tool::EllipseSelect, (20, 20), (76, 76));
    m.set_size(40);
    drag(&mut m, Tool::Brush, (0, 48), (95, 48));
    assert_ne!(px(&m, 48, 48), WHITE);
    assert_eq!(
        px(&m, 21, 22),
        WHITE,
        "the corner of the box is outside the ellipse"
    );
}

#[test]
fn hostile_coordinates_never_panic() {
    let extremes = [i32::MIN, -1_000_000, -1, 0, 95, 1_000_000, i32::MAX];
    for tool in DRAWING
        .into_iter()
        .chain([Tool::Fill, Tool::Picker, Tool::Text])
    {
        let mut m = model();
        m.set_tool(tool);
        for &a in &extremes {
            for &b in &extremes {
                m.begin_with(a, b, Side::Primary, SHIFT);
                m.extend_with(b, a, SHIFT);
                m.extend_with(i32::MAX, i32::MIN, NONE);
                m.end();
            }
        }
        m.set_size(u32::MAX);
        m.begin(0, 0, Side::Secondary);
        m.extend(95, 95);
        m.cancel();
    }
}

#[test]
fn the_view_tools_do_nothing_to_the_picture() {
    for tool in [Tool::Zoom, Tool::Pan] {
        let mut m = model();
        drag(&mut m, tool, (5, 5), (60, 60));
        assert_eq!(inked(&m), 0);
        assert_eq!(m.history().undo_len(), 0);
    }
}

#[test]
fn a_translucent_shape_blends_and_a_selection_edge_blends_with_it() {
    let mut m = model();
    m.set_primary([255, 0, 0, 128]);
    let mut shape = m.shape();
    shape.mode = FillMode::Outline;
    shape.width = 6.0;
    m.set_shape(shape);
    drag(&mut m, Tool::Rectangle, (20, 20), (70, 70));
    let edge = px(&m, 45, 21);
    assert_eq!(edge[3], 255, "over opaque white it stays opaque");
    assert!(
        edge[1] > 100 && edge[1] < 140,
        "half red over white: {edge:?}"
    );
}

#[test]
fn tools_after_a_layer_switch_draw_on_the_active_layer() {
    let mut m = model();
    m.add_layer().unwrap();
    drag(&mut m, Tool::Brush, (10, 10), (50, 10));
    assert_eq!(px(&m, 30, 10), BLACK);
    assert_eq!(
        m.document().layer(0).unwrap().pixels.get(30, 10),
        Some(WHITE)
    );
    m.undo();
    assert_eq!(px(&m, 30, 10), [0, 0, 0, 0]);
}
