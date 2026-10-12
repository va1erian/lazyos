//! The Text tool through the model with a box-drawing fake rasterizer: place,
//! type, edit, move, commit, cancel, undo. The real glyphs are in
//! `tools_view.rs` (feature `text`).

use std::rc::Rc;

use xui_paint::model::text::{LineGeometry, TextAlign, TextCoverage, TextRasterizer, TextStyle};
use xui_paint::model::tools::Coverage;
use xui_paint::model::{Model, Pixel, Rect, Side, TextKey, Tool, WHITE};

const BLACK: Pixel = [0, 0, 0, 255];
const RED: Pixel = [255, 0, 0, 255];

/// Every character is a solid box `size / 2` wide and `size` tall, lines are
/// `size` apart; spaces are blank.
struct Boxes;

impl TextRasterizer for Boxes {
    fn layout_in(&self, text: &str, style: &TextStyle, window: Rect) -> TextCoverage {
        let cw = (style.size / 2.0) as i32;
        let ch = style.size as i32;
        let lines: Vec<&str> = text.split('\n').collect();
        let widest = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0) as i32;
        let (w, h) = ((widest * cw).max(1), (lines.len() as i32 * ch).max(1));
        let mut mask = Coverage::new(Rect::new(0, 0, w as u32, h as u32));
        let mut geometry = Vec::new();
        let mut start = 0;
        for (row, line) in lines.iter().enumerate() {
            for (col, c) in line.chars().enumerate() {
                if c == ' ' {
                    continue;
                }
                for y in 0..ch {
                    for x in 0..cw {
                        let at = ((row as i32 * ch + y) * w + col as i32 * cw + x) as usize;
                        mask.data_mut()[at] = 255;
                    }
                }
            }
            let count = line.chars().count();
            geometry.push(LineGeometry {
                top: (row as i32 * ch) as f32,
                height: ch as f32,
                baseline: (row as i32 * ch) as f32 + ch as f32 * 0.8,
                start,
                carets: (0..=count).map(|i| (i as i32 * cw) as f32).collect(),
            });
            start += count + 1;
        }
        TextCoverage::windowed(mask, window, geometry)
    }

    fn families(&self) -> Vec<String> {
        vec!["Boxes".to_string()]
    }
}

fn model() -> Model {
    let mut m = Model::new(120, 120);
    m.set_text_rasterizer(Rc::new(Boxes));
    let mut style = m.text_style().clone();
    style.size = 10.0;
    m.set_text_style(style);
    m.set_tool(Tool::Text);
    m
}

fn click(m: &mut Model, x: i32, y: i32) {
    m.begin(x, y, Side::Primary);
    m.end();
}

fn type_str(m: &mut Model, text: &str) {
    for c in text.chars() {
        if c == '\n' {
            m.text_key(TextKey::Enter);
        } else {
            m.type_char(c);
        }
    }
}

fn px(m: &Model, x: i32, y: i32) -> Pixel {
    m.bitmap().get(x, y).unwrap()
}

#[test]
fn a_click_places_a_caret_and_typing_shows_live_text_in_the_primary_colour() {
    let mut m = model();
    m.set_primary(RED);
    click(&mut m, 10, 20);
    assert!(m.is_editing_text());
    let (x, y, h) = m.text_caret().expect("a caret");
    assert_eq!((x, y, h), (10.0, 20.0, 10.0));
    type_str(&mut m, "AB");
    assert_eq!(px(&m, 12, 25), RED, "the first box");
    assert_eq!(px(&m, 17, 25), RED, "the second");
    assert_eq!(px(&m, 22, 25), WHITE);
    assert_eq!(m.text_caret().unwrap().0, 20.0, "the caret follows");
    assert_eq!(m.history().undo_len(), 0, "typing is not history yet");
}

#[test]
fn enter_starts_a_new_line_and_the_arrows_home_and_end_move_the_caret() {
    let mut m = model();
    click(&mut m, 10, 10);
    type_str(&mut m, "ab\ncd");
    assert_eq!(px(&m, 12, 25), BLACK, "the second line is below");
    assert_eq!(m.text_caret().unwrap(), (20.0, 20.0, 10.0));
    m.text_key(TextKey::Up);
    assert_eq!(m.text_caret().unwrap(), (20.0, 10.0, 10.0));
    m.text_key(TextKey::Home);
    assert_eq!(m.text_caret().unwrap().0, 10.0);
    m.text_key(TextKey::End);
    assert_eq!(m.text_caret().unwrap().0, 20.0);
    m.text_key(TextKey::Down);
    m.text_key(TextKey::Left);
    assert_eq!(m.text_caret().unwrap(), (15.0, 20.0, 10.0));
    m.text_key(TextKey::Right);
    assert_eq!(m.text_caret().unwrap().0, 20.0);
}

#[test]
fn backspace_and_delete_edit_and_the_pixels_follow() {
    let mut m = model();
    click(&mut m, 10, 10);
    type_str(&mut m, "abc");
    assert_eq!(px(&m, 22, 15), BLACK);
    m.text_key(TextKey::Backspace);
    assert_eq!(px(&m, 22, 15), WHITE, "the last box is gone");
    m.text_key(TextKey::Home);
    m.text_key(TextKey::Delete);
    assert_eq!(px(&m, 17, 15), WHITE, "now only one box is left");
    assert_eq!(px(&m, 12, 15), BLACK);
    // Backspace at the start and Delete at the end do nothing.
    m.text_key(TextKey::Home);
    m.text_key(TextKey::Backspace);
    m.text_key(TextKey::End);
    m.text_key(TextKey::Delete);
    assert_eq!(px(&m, 12, 15), BLACK);
}

#[test]
fn control_characters_are_not_text() {
    let mut m = model();
    click(&mut m, 10, 10);
    m.type_char('\n');
    m.type_char('\u{8}');
    m.type_char('\t');
    m.type_char('\u{7f}');
    assert_eq!(m.text_caret().unwrap().0, 10.0);
    assert_eq!(m.text_caret().unwrap().1, 10.0, "no new line either");
}

#[test]
fn commit_is_one_history_entry_and_undo_removes_the_text() {
    let mut m = model();
    click(&mut m, 10, 10);
    type_str(&mut m, "hello");
    m.text_key(TextKey::Commit);
    assert!(!m.is_editing_text());
    assert_eq!(m.history().undo_len(), 1);
    assert_eq!(m.history().undo_name(), Some("Text"));
    assert_eq!(px(&m, 12, 15), BLACK, "the pixels stayed");
    m.undo();
    assert_eq!(px(&m, 12, 15), WHITE);
    m.redo();
    assert_eq!(px(&m, 12, 15), BLACK);
}

#[test]
fn esc_cancels_and_leaves_no_trace() {
    let mut m = model();
    click(&mut m, 10, 10);
    type_str(&mut m, "hello");
    m.text_key(TextKey::Cancel);
    assert!(!m.is_editing_text());
    assert_eq!(px(&m, 12, 15), WHITE);
    assert_eq!(m.history().undo_len(), 0);
    assert!(m.text_caret().is_none());
}

#[test]
fn a_tool_change_a_click_outside_and_structural_actions_commit() {
    let mut m = model();
    click(&mut m, 10, 10);
    type_str(&mut m, "a");
    m.set_tool(Tool::Brush);
    assert_eq!(m.history().undo_len(), 1, "a tool change commits");
    assert_eq!(px(&m, 12, 15), BLACK);

    m.set_tool(Tool::Text);
    click(&mut m, 10, 50);
    type_str(&mut m, "b");
    click(&mut m, 90, 90);
    assert_eq!(
        m.history().undo_len(),
        2,
        "a click outside commits the first"
    );
    assert_eq!(px(&m, 12, 55), BLACK);
    assert!(m.is_editing_text(), "and starts the next block there");
    assert_eq!(m.text_caret().unwrap().0, 90.0);
    type_str(&mut m, "c");
    m.add_layer().unwrap();
    assert!(!m.is_editing_text(), "adding a layer commits");
    assert_eq!(m.history().undo_len(), 4, "the text, then the layer");
}

#[test]
fn a_click_in_the_block_moves_the_caret_instead_of_starting_a_new_one() {
    let mut m = model();
    click(&mut m, 10, 10);
    type_str(&mut m, "abcd");
    click(&mut m, 22, 14);
    assert!(m.is_editing_text());
    assert_eq!(m.text_caret().unwrap().0, 20.0, "between b and c");
    assert_eq!(m.history().undo_len(), 0);
    type_str(&mut m, "X");
    m.text_key(TextKey::Commit);
    assert_eq!(m.history().undo_len(), 1);
}

#[test]
fn the_handle_drags_the_block_and_the_pixels_move_with_it() {
    let mut m = model();
    click(&mut m, 30, 30);
    type_str(&mut m, "ab");
    m.set_text_handle_px(8);
    let handle = m.text_handle().expect("a handle");
    assert!(
        handle.x1 <= 30 && handle.y1 <= 30,
        "{handle:?} is above-left"
    );
    m.begin(handle.x0 + 2, handle.y0 + 2, Side::Primary);
    m.extend(handle.x0 + 22, handle.y0 + 32);
    assert!(m.is_dragging());
    m.end();
    assert!(m.is_editing_text(), "moving does not commit");
    assert_eq!(px(&m, 32, 35), WHITE, "gone from the old place");
    assert_eq!(px(&m, 52, 65), BLACK, "and drawn at the new one");
    assert_eq!(m.text_caret().unwrap().0, 60.0);
    m.text_key(TextKey::Commit);
    assert_eq!(
        m.history().undo_len(),
        1,
        "the move is part of the one entry"
    );
    m.undo();
    assert_eq!(px(&m, 52, 65), WHITE);
}

#[test]
fn styling_and_colour_changes_update_the_live_text() {
    let mut m = model();
    click(&mut m, 10, 10);
    type_str(&mut m, "a");
    assert_eq!(px(&m, 12, 15), BLACK);
    m.set_primary(RED);
    assert_eq!(px(&m, 12, 15), RED, "a colour change repaints");
    let mut style = m.text_style().clone();
    style.size = 20.0;
    style.align = TextAlign::Left;
    m.set_text_style(style);
    assert_eq!(px(&m, 17, 25), RED, "a bigger box");
    m.swap_colors();
    assert_eq!(px(&m, 17, 25), WHITE, "the secondary (white) now paints");
    assert!(m.is_editing_text());
}

#[test]
fn text_is_clipped_to_the_selection() {
    let mut m = model();
    m.set_tool(Tool::RectSelect);
    m.begin(10, 10, Side::Primary);
    m.extend(25, 40);
    m.end();
    m.set_tool(Tool::Text);
    click(&mut m, 5, 10);
    type_str(&mut m, "abcdef");
    assert_eq!(px(&m, 7, 15), WHITE, "left of the selection");
    assert_eq!(px(&m, 12, 15), BLACK, "inside");
    assert_eq!(px(&m, 40, 15), WHITE, "right of it");
}

#[test]
fn without_fonts_the_tool_says_so_and_does_nothing() {
    let mut m = Model::new(40, 40);
    m.set_tool(Tool::Text);
    click(&mut m, 5, 5);
    assert!(!m.is_editing_text());
    assert!(m.take_notice().is_some());
    m.type_char('x');
    m.text_key(TextKey::Enter);
    assert_eq!(m.history().undo_len(), 0);
}

#[test]
fn an_empty_block_commits_nothing() {
    let mut m = model();
    click(&mut m, 10, 10);
    type_str(&mut m, "   ");
    m.text_key(TextKey::Commit);
    assert_eq!(m.history().undo_len(), 0, "spaces have no ink");
    click(&mut m, 10, 10);
    m.set_tool(Tool::Brush);
    assert_eq!(m.history().undo_len(), 0);
}

#[test]
fn a_huge_amount_of_text_and_hostile_clicks_are_bounded() {
    let mut m = model();
    click(&mut m, i32::MAX, i32::MIN);
    type_str(&mut m, "x");
    m.text_key(TextKey::Commit);
    click(&mut m, 0, 0);
    for _ in 0..1_500 {
        m.type_char('w');
    }
    assert!(m.is_editing_text());
    m.text_key(TextKey::Cancel);
    assert_eq!(m.history().undo_len(), 0);
}
