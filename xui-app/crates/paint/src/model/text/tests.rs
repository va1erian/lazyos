//! The text model against a fake rasterizer: 6 px per character, 10 px lines.

use super::*;
use crate::model::rect::Rect;
use crate::model::tools::Coverage;

struct Fake;

const ADVANCE: f32 = 6.0;
const LINE: f32 = 10.0;

impl TextRasterizer for Fake {
    fn layout_in(&self, text: &str, style: &TextStyle, window: Rect) -> TextCoverage {
        let lines: Vec<&str> = text.split('\n').collect();
        let block = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0) as f32 * ADVANCE;
        let mut mask = Coverage::new(Rect::new(
            0,
            0,
            block as u32 + 2,
            (lines.len() as f32 * LINE) as u32,
        ));
        let mut out = Vec::new();
        let mut start = 0;
        for (i, line) in lines.iter().enumerate() {
            let count = line.chars().count();
            let width = count as f32 * ADVANCE;
            let offset = match style.align {
                TextAlign::Left => 0.0,
                TextAlign::Center => (block - width) / 2.0,
                TextAlign::Right => block - width,
            };
            for (k, c) in line.chars().enumerate() {
                if c == ' ' {
                    continue;
                }
                let x0 = (offset + k as f32 * ADVANCE) as i32;
                for y in 0..LINE as i32 {
                    for x in x0..x0 + ADVANCE as i32 - 1 {
                        let at = (i as i32 * LINE as i32 + y) as usize
                            * mask.rect().width() as usize
                            + x as usize;
                        mask.data_mut()[at] = 255;
                    }
                }
            }
            out.push(LineGeometry {
                top: i as f32 * LINE,
                height: LINE,
                baseline: i as f32 * LINE + 8.0,
                start,
                carets: (0..=count).map(|k| offset + k as f32 * ADVANCE).collect(),
            });
            start += count + 1;
        }
        TextCoverage::windowed(mask, window, out)
    }
}

fn typed(text: &str) -> TextEdit {
    let mut edit = TextEdit::new((10, 20), TextStyle::default());
    edit.insert_str(text);
    edit
}

#[test]
fn typing_moves_the_caret_and_backspace_and_delete_edit() {
    let mut edit = typed("Hello");
    assert_eq!((edit.text().as_str(), edit.caret()), ("Hello", 5));
    edit.backspace();
    assert_eq!(edit.text(), "Hell");
    edit.left();
    edit.left();
    edit.insert('X');
    assert_eq!((edit.text().as_str(), edit.caret()), ("HeXll", 3));
    edit.delete();
    assert_eq!(edit.text(), "HeXl");
    edit.delete();
    edit.delete();
    edit.delete();
    assert_eq!(edit.text(), "HeX", "delete at the end is a no-op");
    let mut empty = TextEdit::new((0, 0), TextStyle::default());
    empty.backspace();
    empty.delete();
    empty.left();
    assert!(empty.is_empty() && empty.caret() == 0);
}

#[test]
fn home_end_and_newlines() {
    let mut edit = typed("ab");
    edit.newline();
    edit.insert_str("cde");
    assert_eq!(edit.text(), "ab\ncde");
    edit.home();
    assert_eq!(edit.caret(), 3);
    edit.end();
    assert_eq!(edit.caret(), 6);
    edit.left();
    edit.left();
    edit.left();
    edit.left();
    assert_eq!(edit.caret(), 2, "left crosses the newline");
    edit.end();
    assert_eq!(
        edit.caret(),
        2,
        "end of the first line stops before the newline"
    );
    edit.home();
    assert_eq!(edit.caret(), 0);
    edit.right();
    edit.right();
    edit.right();
    assert_eq!(edit.caret(), 3);
    // Backspace at the start of a line joins it to the previous one.
    edit.backspace();
    assert_eq!(edit.text(), "abcde");
}

#[test]
fn pasted_text_normalizes_line_ends_and_drops_controls() {
    let mut edit = TextEdit::new((0, 0), TextStyle::default());
    edit.insert_str("a\r\nb\rc\td\u{7}e");
    assert_eq!(edit.text(), "a\nb\ncde");
    edit.insert('\u{0}');
    assert_eq!(edit.text(), "a\nb\ncde");
}

#[test]
fn the_block_has_a_cap() {
    let mut edit = TextEdit::new((0, 0), TextStyle::default());
    for _ in 0..MAX_CHARS + 50 {
        edit.insert('a');
    }
    assert_eq!(edit.text().len(), MAX_CHARS);
}

#[test]
fn moving_the_block_moves_the_commit() {
    let mut edit = typed("Hi");
    assert_eq!(edit.commit(&Fake).unwrap().rect().x0, 10);
    edit.move_by(5, -3);
    assert_eq!(edit.origin(), (15, 17));
    let committed = edit.commit(&Fake).unwrap();
    assert_eq!((committed.rect().x0, committed.rect().y0), (15, 17));
    assert_eq!(committed.get(15, 17), 255);
    edit.move_by(i32::MAX, i32::MIN);
    edit.move_by(i32::MAX, i32::MIN);
    assert!(edit.commit(&Fake).is_some(), "saturates, never panics");
}

#[test]
fn commit_and_cancel() {
    let blank = TextEdit::new((0, 0), TextStyle::default());
    assert!(blank.commit(&Fake).is_none());
    let spaces = typed("   ");
    assert!(spaces.commit(&Fake).is_none(), "no ink, nothing to commit");
    let edit = typed("ab");
    let before = edit.text();
    edit.clone().cancel();
    assert_eq!(edit.text(), before);
}

#[test]
fn caret_geometry_follows_lines_and_alignment() {
    let edit = typed("abcd\nab");
    let (x, y, h) = edit.caret_rect(&Fake).unwrap();
    assert_eq!(
        (x, y, h),
        (10.0 + 12.0, 20.0 + 10.0, 10.0),
        "after 'ab' on line 2, offset by the origin"
    );

    let mut centred = typed("abcd\nab");
    centred.style.align = TextAlign::Center;
    let (x, ..) = centred.caret_rect(&Fake).unwrap();
    assert_eq!(x, 10.0 + 6.0 + 12.0, "the short line is centred");
    centred.style.align = TextAlign::Right;
    let (x, ..) = centred.caret_rect(&Fake).unwrap();
    assert_eq!(x, 10.0 + 12.0 + 12.0);
}

#[test]
fn up_and_down_keep_the_column() {
    let mut edit = typed("abcdef\nab\nabcdef");
    // Caret at the end of line three (column 6).
    edit.up(&Fake);
    assert_eq!(
        edit.caret(),
        9,
        "clamped to the end of the short middle line"
    );
    edit.up(&Fake);
    assert_eq!(edit.caret(), 2);
    edit.up(&Fake);
    assert_eq!(edit.caret(), 2, "no line above");
    edit.down(&Fake);
    edit.down(&Fake);
    edit.down(&Fake);
    assert_eq!(edit.caret(), 12);
}

#[test]
fn clicking_places_the_caret() {
    let mut edit = typed("abcdef\nxyz");
    // Origin (10, 20); a click at x = 10 + 13 is nearest to column 2 (x = 12).
    edit.set_caret_at(&Fake, 10 + 13, 20 + 2);
    assert_eq!(edit.caret(), 2);
    edit.set_caret_at(&Fake, 10 + 100, 20 + 15);
    assert_eq!(edit.caret(), 7 + 3, "past the end of line two");
    edit.set_caret_at(&Fake, -500, -500);
    assert_eq!(edit.caret(), 0);
}

#[test]
fn coverage_geometry_queries() {
    let layout = Fake.layout("ab\ncd", &TextStyle::default());
    assert_eq!(layout.lines.len(), 2);
    assert_eq!(layout.caret(0), Some((0.0, 0.0, 10.0)));
    assert_eq!(layout.caret(2), Some((12.0, 0.0, 10.0)));
    assert_eq!(layout.caret(3), Some((0.0, 10.0, 10.0)));
    assert_eq!(layout.line_column(4), (1, 1));
    assert_eq!(layout.lines[0].len(), 2);
    let empty = Fake.layout("", &TextStyle::default());
    assert_eq!(empty.lines.len(), 1);
    assert!(empty.lines[0].is_empty());
}
