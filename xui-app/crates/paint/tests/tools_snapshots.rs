//! The P4 tools as pictures, light and dark at 96 and 192 dpi: a soft brush,
//! antialiased shapes in every mode, gradients through a selection, text
//! being edited with its caret, and committed text. Each scene is made with
//! real drags and key presses and checked by pixel predicates; the PNGs go to
//! `target/snapshots/paint-p4-*` for eyeballing.

mod common;

use xui_core::Theme;
use xui_core::image::Image;
use xui_core::message::MouseButton;
use xui_paint::Msg;
use xui_paint::model::Tool;
use xui_paint::model::tools::{Arrow, FillMode, GradientKind};
use xui_paint::view::OptMsg;

use common::Areas;
use common::driver::*;
use common::setup::{Look, save, scene};

const RED: [u8; 4] = [200, 30, 30, 255];
const TEAL: [u8; 4] = [20, 130, 160, 255];
const SIZE_BLUE: [u8; 4] = [30, 60, 200, 255];

/// Light and dark at both DPIs.
fn looks() -> Vec<(String, Look)> {
    let mut out = Vec::new();
    for (theme, name) in [(Theme::light(), "light"), (Theme::dark(), "dark")] {
        for dpi in [96, 192] {
            out.push((format!("{name}-{dpi}"), Look::plain().at(theme, dpi)));
        }
    }
    out
}

fn colours(stage: &Stg<'_>, primary: [u8; 4], secondary: [u8; 4]) {
    stage.emit(Msg::Palette {
        color: primary,
        side: xui_paint::model::Side::Primary,
    });
    stage.emit(Msg::Palette {
        color: secondary,
        side: xui_paint::model::Side::Secondary,
    });
}

fn stroke(stage: &Stg<'_>, areas: &Areas, points: &[(i32, i32)]) {
    let at = |p: &(i32, i32)| areas.canvas_at(p.0, p.1);
    let first = at(&points[0]);
    stage.inject(down(first.0, first.1, MouseButton::Left));
    for p in &points[1..] {
        let p = at(p);
        move_to(stage, p);
    }
    let last = at(points.last().unwrap());
    stage.inject(up(last.0, last.1, MouseButton::Left));
}

fn count_not_white(image: &Image, areas: &Areas, from: (i32, i32), to: (i32, i32)) -> usize {
    let (a, b) = (areas.canvas_at(from.0, from.1), areas.canvas_at(to.0, to.1));
    (a.1..b.1)
        .flat_map(|y| (a.0..b.0).map(move |x| (x, y)))
        .filter(|&(x, y)| !is(image, x as u32, y as u32, WHITE))
        .count()
}

#[test]
fn a_soft_brush_at_four_hardnesses() {
    for (name, look) in looks() {
        let (image, areas) = scene(look, |stage, _, parts, _| {
            colours(stage, SIZE_BLUE, WHITE);
            stage.emit(Msg::Tool(Tool::Brush));
            stage.emit(Msg::Size(34));
            for (row, hardness) in [100u8, 60, 25, 0].into_iter().enumerate() {
                stage.emit(Msg::Opt(OptMsg::Hardness(hardness)));
                let y = 35 + row as i32 * 55;
                let wave: Vec<(i32, i32)> = (0..=12)
                    .map(|i| (30 + i * 21, y + ((i as f32 * 0.9).sin() * 8.0) as i32))
                    .collect();
                stroke(stage, &parts.areas, &wave);
            }
            stage.emit(Msg::Tool(Tool::Pan));
        });
        save(&format!("paint-p4-soft-brush-{name}"), &image);
        // A hard stroke is solid across its width; the softest is mostly fade.
        let solid = |row: i32| {
            let (a, b) = (
                areas.canvas_at(10, 10 + row * 55),
                areas.canvas_at(310, 62 + row * 55),
            );
            (a.1..b.1)
                .flat_map(|y| (a.0..b.0).map(move |x| (x, y)))
                .filter(|&(x, y)| is(&image, x as u32, y as u32, SIZE_BLUE))
                .count()
        };
        assert!(
            solid(0) > 2 * solid(3),
            "{name}: {} vs {}",
            solid(0),
            solid(3)
        );
    }
}

#[test]
fn antialiased_shapes_in_every_mode() {
    for (name, look) in looks() {
        let (image, areas) = scene(look, |stage, _, parts, _| {
            colours(stage, RED, TEAL);
            stage.emit(Msg::Size(5));
            let areas = &parts.areas;
            let tools = [
                Tool::Line,
                Tool::Rectangle,
                Tool::RoundedRectangle,
                Tool::Ellipse,
            ];
            for (row, mode) in [FillMode::Outline, FillMode::Fill, FillMode::Both]
                .into_iter()
                .enumerate()
            {
                stage.emit(Msg::Opt(OptMsg::ShapeMode(mode)));
                for (col, tool) in tools.into_iter().enumerate() {
                    stage.emit(Msg::Tool(tool));
                    if tool == Tool::Line {
                        let arrow = [Arrow::None, Arrow::End, Arrow::Both][row];
                        stage.emit(Msg::Opt(OptMsg::Arrow(arrow)));
                    }
                    let (x, y) = (8 + col as i32 * 62, 10 + row as i32 * 76);
                    stroke(stage, areas, &[(x, y), (x + 30, y + 22), (x + 50, y + 54)]);
                }
            }
            // An aliased ellipse, for comparison.
            stage.emit(Msg::Tool(Tool::Ellipse));
            stage.emit(Msg::Opt(OptMsg::Antialias(false)));
            stroke(stage, areas, &[(258, 20), (310, 74)]);
            stage.emit(Msg::Tool(Tool::Pan));
        });
        save(&format!("paint-p4-shapes-{name}"), &image);
        assert!(
            count_not_white(&image, &areas, (10, 10), (310, 230)) > 3000,
            "{name}"
        );
        // Filled and both modes put the secondary in the middle of a box.
        let middle = areas.canvas_at(70 + 25, 86 + 27);
        assert!(
            is(&image, middle.0 as u32, middle.1 as u32, TEAL),
            "{name}: fill"
        );
    }
}

#[test]
fn gradients_through_a_selection_and_one_in_progress() {
    for (name, look) in looks() {
        let (image, areas) = scene(look, |stage, _, parts, _| {
            colours(stage, [20, 20, 120, 255], [250, 220, 60, 255]);
            let areas = &parts.areas;
            let kinds = [
                GradientKind::Linear,
                GradientKind::LinearReflected,
                GradientKind::Radial,
                GradientKind::Diamond,
            ];
            for (i, kind) in kinds.into_iter().enumerate() {
                let (x, y) = (10 + (i as i32 % 2) * 150, 10 + (i as i32 / 2) * 115);
                stage.emit(Msg::Tool(Tool::RectSelect));
                stroke(stage, areas, &[(x, y), (x + 139, y + 104)]);
                stage.emit(Msg::Tool(Tool::Gradient));
                stage.emit(Msg::Opt(OptMsg::GradientKind(kind)));
                if i == 3 {
                    // The last is still being dragged: the live preview.
                    let from = areas.canvas_at(x + 70, y + 52);
                    stage.inject(down(from.0, from.1, MouseButton::Left));
                    move_to(stage, areas.canvas_at(x + 130, y + 95));
                } else if i == 0 {
                    stroke(stage, areas, &[(x + 10, y + 10), (x + 120, y + 90)]);
                } else {
                    // From the middle, so the other side shows too.
                    stroke(stage, areas, &[(x + 70, y + 52), (x + 130, y + 95)]);
                }
            }
        });
        save(&format!("paint-p4-gradients-{name}"), &image);
        for (x, y) in [(30, 30), (180, 30), (30, 145), (180, 145)] {
            let p = areas.canvas_at(x, y);
            assert!(
                !is(&image, p.0 as u32, p.1 as u32, WHITE),
                "{name}: ({x},{y})"
            );
        }
        let gap = areas.canvas_at(155, 20);
        assert!(
            is(&image, gap.0 as u32, gap.1 as u32, WHITE),
            "{name}: between the boxes"
        );
    }
}

#[cfg(feature = "text")]
mod text {
    use super::*;
    use common::setup::liberation;
    use xui_core::backend::Event;
    use xui_core::message::{Key, Modifiers};

    fn typed(stage: &Stg<'_>, text: &str) {
        for c in text.chars() {
            if c == '\n' {
                press(stage, Key::RETURN);
            } else {
                stage.inject(Event::Char(c));
            }
        }
    }

    #[test]
    fn text_being_edited_shows_its_caret_and_block() {
        for (name, look) in looks() {
            let (image, areas) =
                scene(look.with_text(liberation()), |stage, observer, parts, _| {
                    colours(stage, [10, 10, 10, 255], WHITE);
                    stage.emit(Msg::Tool(Tool::Text));
                    let (x, y) = parts.areas.canvas_at(30, 40);
                    click(stage, (x, y), MouseButton::Left);
                    typed(stage, "Hello,\nPaint");
                    assert!(observer.borrow().text_editing);
                });
            save(&format!("paint-p4-text-editing-{name}"), &image);
            assert!(
                count_not_white(&image, &areas, (28, 38), (200, 110)) > 400,
                "{name}"
            );
        }
    }

    #[test]
    fn committed_text_in_three_styles() {
        for (name, look) in looks() {
            let (image, areas) =
                scene(look.with_text(liberation()), |stage, observer, parts, _| {
                    colours(stage, [10, 10, 10, 255], WHITE);
                    let areas = &parts.areas;
                    stage.emit(Msg::Tool(Tool::Text));
                    for (row, (bold, italic, underline)) in [
                        (false, false, false),
                        (true, false, true),
                        (false, true, false),
                    ]
                    .into_iter()
                    .enumerate()
                    {
                        stage.emit(Msg::Opt(OptMsg::FontSize(26)));
                        stage.emit(Msg::Opt(OptMsg::Bold(bold)));
                        stage.emit(Msg::Opt(OptMsg::Italic(italic)));
                        stage.emit(Msg::Opt(OptMsg::Underline(underline)));
                        let (x, y) = areas.canvas_at(20, 15 + row as i32 * 60);
                        click(stage, (x, y), MouseButton::Left);
                        typed(stage, "Paint text");
                        let ctrl_enter = Modifiers {
                            ctrl: true,
                            ..Modifiers::NONE
                        };
                        press_with(stage, Key::RETURN, ctrl_enter);
                    }
                    assert_eq!(observer.borrow().history.0, 3);
                });
            save(&format!("paint-p4-text-committed-{name}"), &image);
            assert!(
                count_not_white(&image, &areas, (18, 12), (300, 200)) > 1500,
                "{name}"
            );
        }
    }
}
