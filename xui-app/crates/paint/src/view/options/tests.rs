//! Width stepping and which controls each tool shows.

use super::*;

#[test]
fn stepping_walks_the_size_list_and_clamps_at_the_ends() {
    assert_eq!(step_size(4, 1), 5);
    assert_eq!(step_size(4, -1), 3);
    assert_eq!(step_size(500, 1), 500, "no larger size than the last");
    assert_eq!(step_size(1, -1), 1, "no smaller size than the first");
    assert_eq!(step_size(4, 0), 4);
}

#[test]
fn a_size_between_entries_steps_to_the_next_one_in_that_direction() {
    assert_eq!(step_size(7, 1), 8);
    assert_eq!(step_size(7, -1), 6);
    assert_eq!(step_size(1000, 1), 500);
    assert_eq!(step_size(1000, -1), 500);
}

#[test]
fn a_wide_brush_keeps_stepping_up_to_the_widest() {
    let mut size = 1;
    for _ in 0..SIZES.len() {
        size = step_size(size, 1);
    }
    assert_eq!(size, 500);
    assert_eq!(SIZES.last().copied(), Some(MAX_BRUSH_WIDTH));
}

#[test]
fn only_tools_that_paint_with_a_width_offer_the_size() {
    for tool in [
        Tool::Brush,
        Tool::Eraser,
        Tool::Line,
        Tool::Rectangle,
        Tool::RoundedRectangle,
        Tool::Ellipse,
    ] {
        assert!(uses_size(tool), "{tool:?}");
    }
    for tool in [Tool::Pencil, Tool::Fill, Tool::Picker, Tool::Gradient, Tool::Text] {
        assert!(!uses_size(tool), "{tool:?}");
    }
}

#[test]
fn each_tool_shows_its_own_groups_and_no_others() {
    let shown = |tool| Shown::of(tool);
    assert!(shown(Tool::Brush).hardness && shown(Tool::Brush).antialias);
    assert!(shown(Tool::Eraser).hardness);
    assert!(shown(Tool::Line).arrow && !shown(Tool::Line).style);
    assert!(shown(Tool::Rectangle).style && !shown(Tool::Rectangle).radius);
    assert!(shown(Tool::RoundedRectangle).radius);
    assert!(shown(Tool::Ellipse).style);
    assert!(shown(Tool::Fill).bucket && !shown(Tool::Fill).size);
    assert!(shown(Tool::Picker).picker);
    assert!(shown(Tool::Gradient).gradient);
    assert!(shown(Tool::Text).text && shown(Tool::Text).antialias);
    assert!(shown(Tool::MagicWand).wand && shown(Tool::MagicWand).select_mode);
    for tool in [Tool::Pencil, Tool::Zoom, Tool::Pan, Tool::MovePixels] {
        assert_eq!(shown(tool), Shown::default(), "{tool:?} has no options");
    }
}

#[test]
fn every_tool_is_covered_by_the_table() {
    // `Shown::of` is an exhaustive match, so a new tool cannot be forgotten;
    // this walks them to keep the table and `Tool::ALL` honest.
    for tool in Tool::ALL {
        let _ = Shown::of(tool);
    }
}

#[test]
fn option_messages_reach_the_model() {
    let mut model = Model::new(8, 8);
    let families = vec!["Alpha".to_string(), "Beta".to_string()];
    let apply = |msg: OptMsg, model: &mut Model| msg.apply(model, &families);
    apply(OptMsg::Hardness(30), &mut model);
    assert_eq!(model.brush().hardness, 30);
    model.set_tool(Tool::Rectangle);
    apply(OptMsg::Antialias(false), &mut model);
    assert!(!model.shape().antialias);
    assert!(model.brush().antialias, "the brush keeps its own");
    apply(OptMsg::ShapeMode(FillMode::Both), &mut model);
    apply(OptMsg::Radius(9), &mut model);
    apply(OptMsg::Arrow(Arrow::Both), &mut model);
    assert_eq!(model.shape().mode, FillMode::Both);
    assert_eq!(model.shape().radius, 9.0);
    assert_eq!(model.shape().arrow, Arrow::Both);
    apply(OptMsg::FillTolerance(40), &mut model);
    apply(OptMsg::FillArea(FillArea::Global), &mut model);
    apply(OptMsg::FillSample(Sample::Image), &mut model);
    assert_eq!(model.fill().tolerance, 40);
    assert_eq!(model.fill().area, FillArea::Global);
    assert_eq!(model.fill().sample, Sample::Image);
    apply(OptMsg::PickSize(SampleSize::Five), &mut model);
    apply(OptMsg::PickSample(Sample::Image), &mut model);
    apply(OptMsg::PickSwitchBack(true), &mut model);
    assert_eq!(model.picker().size, SampleSize::Five);
    assert!(model.picker().switch_back);
    apply(OptMsg::GradientKind(GradientKind::Diamond), &mut model);
    assert_eq!(model.gradient().kind, GradientKind::Diamond);
    model.set_tool(Tool::Text);
    apply(OptMsg::Family(1), &mut model);
    apply(OptMsg::FontSize(33), &mut model);
    apply(OptMsg::Bold(true), &mut model);
    apply(OptMsg::Italic(true), &mut model);
    apply(OptMsg::Underline(true), &mut model);
    apply(OptMsg::Align(TextAlign::Right), &mut model);
    apply(OptMsg::Antialias(false), &mut model);
    let style = model.text_style();
    assert_eq!(style.family, "Beta");
    assert_eq!(style.size, 33.0);
    assert!(style.bold && style.italic && style.underline && !style.antialias);
    assert_eq!(style.align, TextAlign::Right);
    // An index past the list changes nothing.
    apply(OptMsg::Family(9), &mut model);
    assert_eq!(model.text_style().family, "Beta");
}

#[test]
fn the_width_is_one_number_for_brush_and_shapes() {
    let mut model = Model::new(8, 8);
    model.set_size(40);
    assert_eq!((model.brush().width, model.shape().width), (40, 40.0));
    model.set_size(9999);
    assert_eq!(model.size(), 500, "clamped to the widest brush");
    model.set_size(0);
    assert_eq!(model.size(), 1);
}
