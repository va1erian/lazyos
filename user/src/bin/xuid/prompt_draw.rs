//! Painting the trusted prompt (`prompt.rs`): the screen dimmed under it and
//! a centred panel with the asker, the change, the administrator's name and
//! password fields, and Cancel (the default) and Approve. The layout is one
//! function ([`layout`]) shared by the painter and the pointer's hit test,
//! so a click lands where the button is drawn.

use alloc::string::String;
use alloc::vec::Vec;
use user::messenger::display::{Canvas, Color, Face, Rect};

use super::prompt::{Focus, Prompt};
use super::theme::{accent, overlay_bg, overlay_border, overlay_selected, overlay_text, px};

/// The panel's design size.
const PANEL_W: i32 = 480;
const PANEL_H: i32 = 240;
/// Width of the field captions.
const CAPTION_W: i32 = 112;
/// A button's design size.
const BUTTON_W: i32 = 100;
const BUTTON_H: i32 = 26;
/// Pixels darkened per pass of the dimming (a stack buffer, no allocation).
const DIM_CHUNK: usize = 256;

/// What a pointer press on the prompt hit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Target {
    Name,
    Password,
    Cancel,
    Approve,
}

/// Where everything sits on a screen of `dims`.
struct Layout {
    panel: Rect,
    name: Rect,
    password: Rect,
    cancel: Rect,
    approve: Rect,
}

fn layout(dims: (i32, i32)) -> Layout {
    let (w, h) = (px(PANEL_W), px(PANEL_H));
    let panel = Rect::new((dims.0 - w) / 2, (dims.1 - h) / 2, w, h);
    let field_x = panel.x + px(16) + px(CAPTION_W);
    let field_w = panel.w - px(32) - px(CAPTION_W);
    let row = |y: i32| Rect::new(field_x, panel.y + px(y), field_w, px(24));
    let button_y = panel.y + panel.h - px(16) - px(BUTTON_H);
    let approve = Rect::new(
        panel.x + panel.w - px(16) - px(BUTTON_W),
        button_y,
        px(BUTTON_W),
        px(BUTTON_H),
    );
    let cancel = Rect::new(
        approve.x - px(12) - px(BUTTON_W),
        button_y,
        px(BUTTON_W),
        px(BUTTON_H),
    );
    Layout {
        panel,
        name: row(130),
        password: row(162),
        cancel,
        approve,
    }
}

/// The panel's rectangle (what typing repaints).
pub(super) fn panel(dims: (i32, i32)) -> Rect {
    layout(dims).panel
}

/// What the pointer at `at` would press.
pub(super) fn hit(dims: (i32, i32), at: (i32, i32)) -> Option<Target> {
    let layout = layout(dims);
    let inside = |rect: Rect| {
        at.0 >= rect.x && at.0 < rect.x + rect.w && at.1 >= rect.y && at.1 < rect.y + rect.h
    };
    [
        (layout.name, Target::Name),
        (layout.password, Target::Password),
        (layout.cancel, Target::Cancel),
        (layout.approve, Target::Approve),
    ]
    .into_iter()
    .find(|(rect, _)| inside(*rect))
    .map(|(_, target)| target)
}

/// Paint the prompt over `clip`: the dimmed scene, then the panel.
pub(super) fn draw(screen: &mut Canvas, prompt: &Prompt, clip: Rect) {
    let dims = (screen.width(), screen.height());
    let layout = layout(dims);
    dim(screen, clip, layout.panel);
    let panel = layout.panel;
    if panel.intersect(clip).is_empty() {
        return;
    }
    let ink = overlay_text();
    screen.fill(panel, clip, overlay_bg());
    frame(screen, panel, overlay_border(), px(2), clip);
    // The header band in the accent colour: the compositor's own chrome.
    let header = Rect::new(panel.x, panel.y, panel.w, px(28));
    screen.fill(header, clip, accent());
    let white = Color::rgb(255, 255, 255);
    let text_clip = panel.intersect(clip);
    screen.text_face(
        panel.x + px(14),
        panel.y + (px(28) - Face::Sans.height()) / 2,
        "Administrator approval",
        Face::Sans,
        white,
        text_clip,
    );
    let width = panel.w - px(32);
    let mut lines: Vec<(String, Color)> = Vec::new();
    for line in wrap(&prompt.asker, width, 2) {
        lines.push((line, ink));
    }
    for line in wrap(&alloc::format!("asks to: {}", prompt.summary), width, 2) {
        lines.push((line, ink));
    }
    if !prompt.error.is_empty() {
        lines.push((prompt.error.clone(), Color::rgb(230, 80, 70)));
    }
    let line_h = Face::Sans.height() + px(3);
    for (index, (text, color)) in lines.iter().take(5).enumerate() {
        screen.text_face(
            panel.x + px(16),
            panel.y + px(36) + index as i32 * line_h,
            text,
            Face::Sans,
            *color,
            text_clip,
        );
    }
    let masked: String = prompt.secret.chars().map(|_| '*').collect();
    for (caption, rect, text, focus) in [
        (
            "Administrator",
            layout.name,
            prompt.name.as_str(),
            Focus::Name,
        ),
        (
            "Password",
            layout.password,
            masked.as_str(),
            Focus::Password,
        ),
    ] {
        screen.text_face(
            panel.x + px(16),
            rect.y + (rect.h - Face::Sans.height()) / 2,
            caption,
            Face::Sans,
            ink,
            text_clip,
        );
        field(screen, rect, text, prompt.focus == focus, clip);
    }
    button(
        screen,
        layout.cancel,
        "Cancel",
        true,
        prompt.focus == Focus::Cancel,
        clip,
    );
    button(
        screen,
        layout.approve,
        "Approve",
        false,
        prompt.focus == Focus::Approve,
        clip,
    );
}

/// Darken everything in `clip` outside `panel` to a third of its brightness:
/// the scene stays recognisable but is plainly not live.
fn dim(screen: &mut Canvas, clip: Rect, panel: Rect) {
    let mut buffer = [0u8; DIM_CHUNK * 4];
    let clip = clip.intersect(Rect::new(0, 0, screen.width(), screen.height()));
    for y in clip.y..clip.y + clip.h {
        let mut x = clip.x;
        while x < clip.x + clip.w {
            let w = (clip.x + clip.w - x).min(DIM_CHUNK as i32);
            let span = Rect::new(x, y, w, 1);
            x += w;
            if span.intersect(panel) == span {
                continue;
            }
            let saved = screen.save(span, &mut buffer);
            let bytes = &mut buffer[..saved.w as usize * 4];
            for pixel in bytes.as_chunks_mut::<4>().0 {
                for channel in &mut pixel[..3] {
                    *channel /= 3;
                }
            }
            screen.restore(saved, bytes);
        }
    }
}

/// A text field: a light box, its text and, when focused, the accent frame
/// and a caret.
fn field(screen: &mut Canvas, rect: Rect, text: &str, focused: bool, clip: Rect) {
    let box_color = Color::rgb(250, 250, 250);
    let dark = Color::rgb(20, 20, 20);
    screen.fill(rect, clip, box_color);
    let (edge, thickness) = if focused {
        (accent(), px(2))
    } else {
        (overlay_border(), px(1))
    };
    frame(screen, rect, edge, thickness, clip);
    let inner = Rect::new(rect.x + px(4), rect.y, rect.w - px(8), rect.h).intersect(clip);
    let y = rect.y + (rect.h - Face::Sans.height()) / 2;
    screen.text_face(rect.x + px(6), y, text, Face::Sans, dark, inner);
    if focused {
        let caret_x = (rect.x + px(6) + Face::Sans.width(text)).min(rect.x + rect.w - px(4));
        screen.fill(
            Rect::new(caret_x, rect.y + px(4), px(1), rect.h - px(8)),
            clip,
            dark,
        );
    }
}

/// A button: `default` gets the heavier frame (Cancel), `focused` the
/// highlight.
fn button(screen: &mut Canvas, rect: Rect, label: &str, default: bool, focused: bool, clip: Rect) {
    let fill = if focused {
        overlay_selected()
    } else {
        overlay_bg()
    };
    screen.fill(rect, clip, fill);
    let thickness = if default { px(2) } else { px(1) };
    let edge = if focused { accent() } else { overlay_border() };
    frame(screen, rect, edge, thickness, clip);
    let x = rect.x + (rect.w - Face::Sans.width(label)) / 2;
    let y = rect.y + (rect.h - Face::Sans.height()) / 2;
    screen.text_face(
        x,
        y,
        label,
        Face::Sans,
        overlay_text(),
        rect.intersect(clip),
    );
}

/// A frame `t` pixels thick inside `rect`.
fn frame(screen: &mut Canvas, rect: Rect, color: Color, t: i32, clip: Rect) {
    screen.fill(Rect::new(rect.x, rect.y, rect.w, t), clip, color);
    screen.fill(
        Rect::new(rect.x, rect.y + rect.h - t, rect.w, t),
        clip,
        color,
    );
    screen.fill(Rect::new(rect.x, rect.y, t, rect.h), clip, color);
    screen.fill(
        Rect::new(rect.x + rect.w - t, rect.y, t, rect.h),
        clip,
        color,
    );
}

/// `text` broken into at most `max` lines of `width` pixels at spaces; the
/// last line keeps whatever is left (clipped when painted).
fn wrap(text: &str, width: i32, max: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut line = String::new();
    for word in text.split(' ') {
        let candidate = if line.is_empty() {
            String::from(word)
        } else {
            alloc::format!("{line} {word}")
        };
        if Face::Sans.width(&candidate) <= width || line.is_empty() || lines.len() + 1 >= max {
            line = candidate;
        } else {
            lines.push(core::mem::replace(&mut line, String::from(word)));
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}
