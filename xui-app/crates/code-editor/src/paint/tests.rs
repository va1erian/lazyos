//! Tests for the painter: token visibility windows, grapheme clusters, and the
//! rendered selection, caret, syntax colours and marker squiggle.

use super::paint;
use super::text::{clusters, drawn_cols, whole_clusters};
use crate::markers::{Marker, MarkerKind};
use crate::options::Options;
use crate::platform::InProcessClipboard;
use crate::state::EditorState;
use crate::theme::EditorTheme;
use xui_canvas::{RgbaImage, Surface};
use xui_core::Color;
use xui_core::geometry::Rect;
use xui_core::theme::Theme;

#[test]
fn only_the_visible_columns_of_a_long_token_are_drawn() {
    // A 10 000-column token with columns 100..180 on screen.
    assert_eq!(drawn_cols(0..10_000, 100, 180), 99..181);
    // A token entirely inside the view is drawn whole.
    assert_eq!(drawn_cols(120..130, 100, 180), 120..130);
    // At the left edge nothing underflows.
    assert_eq!(drawn_cols(0..5, 0, 80), 0..5);
    // A token past the view draws nothing.
    assert!(drawn_cols(500..600, 100, 180).is_empty());
    // A token ending at the view's left edge keeps its last column, whose
    // glyph can overhang into view; one ending further left draws nothing.
    assert_eq!(drawn_cols(90..100, 100, 180), 99..100);
    assert!(drawn_cols(90..99, 100, 180).is_empty());
}

#[test]
fn combining_marks_and_joined_characters_are_drawn_with_their_base() {
    let chars = |text: &str| text.chars().collect::<Vec<_>>();
    // "e" + combining acute, then "x": the accent stays with the "e", and
    // the "x" keeps its own column (2, since the accent takes one).
    assert_eq!(
        clusters(&chars("e\u{0301}x")),
        [(0, "e\u{0301}".to_owned()), (2, "x".to_owned())]
    );
    // A ZWJ sequence is one run.
    let family = "\u{1F468}\u{200D}\u{1F469}";
    assert_eq!(clusters(&chars(family)), [(0, family.to_owned())]);
    // Plain ASCII is one cell per character.
    assert_eq!(
        clusters(&chars("ab")),
        [(0, "a".to_owned()), (1, "b".to_owned())]
    );
}

#[test]
fn a_cluster_cut_by_the_left_edge_is_drawn_whole() {
    // "xe" + two combining marks + "y": columns 0..5.
    let line: Vec<char> = "xe\u{0301}\u{0323}y".chars().collect();
    // Scrolled so the drawn range would start on the second mark (col 3):
    // it moves back to the "e" at col 1, not past the token start.
    assert_eq!(whole_clusters(&line, 3..5, 0..5), 1..5);
    assert_eq!(
        whole_clusters(&line, 3..5, 2..5),
        2..5,
        "bounded by the token"
    );
    // A range ending inside a cluster takes the trailing marks too.
    assert_eq!(whole_clusters(&line, 0..2, 0..5), 0..4);
    // Plain text is unchanged.
    let plain: Vec<char> = "abcdef".chars().collect();
    assert_eq!(whole_clusters(&plain, 2..4, 0..6), 2..4);
}

fn render(state: &EditorState) -> RgbaImage {
    let xui_theme = Theme::light();
    let editor_theme = EditorTheme::from_theme(xui_theme);
    let mut surface = Surface::new(240, 80);
    surface.with_canvas_at(Rect::new(0, 0, 240, 80), 96, |canvas| {
        paint(canvas, state, &editor_theme, &xui_theme);
    });
    surface.to_image()
}

fn editor_state(text: &str) -> EditorState {
    let mut state = EditorState::new(text, Options::default(), Box::new(InProcessClipboard));
    state.focused = true;
    state
}

/// An editor state that highlights Rhai, for the token-colour tests.
#[cfg(feature = "rhai-syntax")]
fn rhai_editor_state(text: &str) -> EditorState {
    let mut state = EditorState::with_highlighter(
        text,
        Options::default(),
        Box::new(InProcessClipboard),
        Box::new(crate::lexer::RhaiHighlighter),
    );
    state.focused = true;
    state
}

fn count_color(image: &RgbaImage, color: Color) -> usize {
    let mut count = 0;
    for y in 0..image.height {
        for x in 0..image.width {
            if image.pixel(x, y) == Some([color.r, color.g, color.b, 0xFF]) {
                count += 1;
            }
        }
    }
    count
}

/// How close the nearest painted pixel gets to `color`, in RGB distance.
#[cfg(feature = "rhai-syntax")]
fn nearest_distance(image: &RgbaImage, color: Color) -> f32 {
    let mut best = f32::MAX;
    for y in 0..image.height {
        for x in 0..image.width {
            let Some([r, g, b, _]) = image.pixel(x, y) else {
                continue;
            };
            let distance = (f32::from(r) - f32::from(color.r)).powi(2)
                + (f32::from(g) - f32::from(color.g)).powi(2)
                + (f32::from(b) - f32::from(color.b)).powi(2);
            best = best.min(distance.sqrt());
        }
    }
    best
}

#[test]
fn text_and_selection_are_painted() {
    let mut state = editor_state("hello\nworld");
    state.view.anchor = 0;
    state.view.caret = 5;
    let theme = EditorTheme::from_theme(Theme::light());
    let image = render(&state);

    assert!(
        count_color(&image, theme.selection) > 0,
        "the selection fill is drawn"
    );
    let blank = render(&editor_state(""));
    assert_ne!(
        image.pixels, blank.pixels,
        "a selected document differs from a blank one"
    );
}

#[test]
fn plain_text_is_painted() {
    // The default `PlainText` highlighter emits no tokens; the painter must
    // still draw the line. Unfocused and with the caret off, an empty buffer
    // paints only the background and border, so any difference is glyphs.
    let mut with_text = editor_state("hello");
    with_text.focused = false;
    with_text.blink_on = false;
    let mut blank = editor_state("");
    blank.focused = false;
    blank.blink_on = false;
    assert_ne!(
        render(&with_text).pixels,
        render(&blank).pixels,
        "a plain-text line is still painted"
    );
}

#[test]
fn the_caret_is_painted_only_when_visible() {
    let mut state = editor_state("abc");
    state.view.caret = 1;
    state.view.anchor = 1;
    state.blink_on = true;
    let shown = render(&state);
    state.blink_on = false;
    let hidden = render(&state);
    assert_ne!(shown.pixels, hidden.pixels, "the blink hides the caret");
}

#[cfg(feature = "rhai-syntax")]
#[test]
fn syntax_classes_are_painted_in_their_theme_colours() {
    let state = rhai_editor_state("let total = 42;");
    let theme = EditorTheme::from_theme(Theme::light());
    let image = render(&state);
    let keyword = nearest_distance(&image, theme.keyword);
    let number = nearest_distance(&image, theme.number);
    assert!(
        keyword < 60.0,
        "the keyword is painted in the keyword colour (distance {keyword})"
    );
    assert!(
        number < 60.0,
        "the number is painted in the number colour (distance {number})"
    );
}

#[test]
fn markers_paint_a_squiggle() {
    let mut state = editor_state("let x = ;");
    let plain = render(&state);
    state.markers = vec![Marker::new(0, 8, 8, MarkerKind::Error)];
    let marked = render(&state);
    assert_ne!(
        plain.pixels, marked.pixels,
        "the squiggle changes the rendered pixels"
    );
}
