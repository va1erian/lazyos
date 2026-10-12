//! The preferences panel (Ctrl+,): the Terminal's settings as controls, drawn
//! inside the window over the grid. It is a pure model: keys and clicks go in,
//! `confd` writes (`Change`s) come out. The Terminal stores them under
//! `user/<uid>/term/` and the settings follow the change topic like any other
//! writer's (`rhai`, the Config app), so there is no Apply button and nothing
//! here touches the engine.
//!
//! An in-window panel rather than a second window: the Terminal's window is
//! the one a `confd` change must re-lay out live, a second surface would have
//! to be created, sized, focused and closed with it, and the pty must keep
//! draining while it is open, which is natural when the panel is just another
//! layer of the one painter.

use lazyterm::palette::NAMES;
use lazyterm::settings::{
    parse_grid, Bell, CursorShape, Settings, Value, MAX_SCROLLBACK, MAX_SIZE, MAX_SPACING, MIN_SIZE, MIN_SPACING,
};
use xui_core::{Key, Modifiers, Rect};

use super::geometry::contains;

/// A control that can hold the keyboard focus, in Tab order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Ctl {
    Family,
    Size,
    Spacing,
    Scheme,
    CursorShape,
    Blink,
    Bold,
    Scrollback,
    Bell,
    CopyOnSelect,
    ConfirmPaste,
    WordChars,
    WindowSize,
}

pub const ORDER: [Ctl; 13] = [
    Ctl::Family,
    Ctl::Size,
    Ctl::Spacing,
    Ctl::Scheme,
    Ctl::CursorShape,
    Ctl::Blink,
    Ctl::Bold,
    Ctl::Scrollback,
    Ctl::Bell,
    Ctl::CopyOnSelect,
    Ctl::ConfirmPaste,
    Ctl::WordChars,
    Ctl::WindowSize,
];

const SHAPES: [CursorShape; 3] = [CursorShape::Block, CursorShape::Underline, CursorShape::Bar];
const BELLS: [Bell; 4] = [Bell::None, Bell::Visual, Bell::Sound, Bell::Notify];

/// One write the user's edit asks for: a key under `user/<uid>/term/`.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Change {
    pub key: &'static str,
    pub value: Value,
}

/// What an event did.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Outcome {
    pub close: bool,
    pub changes: Vec<Change>,
}

impl Outcome {
    fn change(key: &'static str, value: Value) -> Outcome {
        Outcome { close: false, changes: vec![Change { key, value }] }
    }
}

/// A text field being edited.
#[derive(Clone, Debug, Default)]
pub struct Field {
    pub text: String,
    pub caret: usize,
}

impl Field {
    fn set(text: &str) -> Field {
        Field { text: text.to_string(), caret: text.chars().count() }
    }

    fn byte(&self, chars: usize) -> usize {
        self.text.char_indices().nth(chars).map_or(self.text.len(), |(i, _)| i)
    }

    /// Apply an editing key; returns whether it was one.
    fn edit(&mut self, key: Key) -> bool {
        let len = self.text.chars().count();
        match key {
            Key::BACK if self.caret > 0 => {
                self.caret -= 1;
                let at = self.byte(self.caret);
                self.text.remove(at);
            }
            Key::DELETE if self.caret < len => {
                let at = self.byte(self.caret);
                self.text.remove(at);
            }
            Key::LEFT => self.caret = self.caret.saturating_sub(1),
            Key::RIGHT => self.caret = (self.caret + 1).min(len),
            Key::HOME => self.caret = 0,
            Key::END => self.caret = len,
            Key::BACK | Key::DELETE => {}
            _ => return false,
        }
        true
    }

    fn insert(&mut self, c: char, limit: usize) {
        if c.is_control() || self.text.chars().count() >= limit {
            return;
        }
        let at = self.byte(self.caret);
        self.text.insert(at, c);
        self.caret += 1;
    }
}

pub struct Prefs {
    /// The settings as last seen (what the controls show).
    pub settings: Settings,
    pub families: Vec<String>,
    pub focus: Ctl,
    pub scrollback: Field,
    pub word_chars: Field,
    pub window_size: Field,
    /// A refused value, shown under the panel title until the next edit.
    pub error: Option<&'static str>,
}

/// How many characters a text field takes.
const FIELD_LIMIT: usize = 64;

impl Prefs {
    pub fn new(settings: &Settings, families: Vec<String>) -> Prefs {
        let mut p = Prefs {
            settings: settings.clone(),
            families,
            focus: Ctl::Family,
            scrollback: Field::default(),
            word_chars: Field::default(),
            window_size: Field::default(),
            error: None,
        };
        p.show(settings);
        p
    }

    /// Show settings that changed elsewhere, without disturbing the field being
    /// edited.
    pub fn sync(&mut self, settings: &Settings) {
        let editing = self.focus;
        self.settings = settings.clone();
        let keep = |ctl: Ctl, field: &Field| (editing == ctl).then(|| field.clone());
        let (a, b, c) = (
            keep(Ctl::Scrollback, &self.scrollback),
            keep(Ctl::WordChars, &self.word_chars),
            keep(Ctl::WindowSize, &self.window_size),
        );
        self.show(settings);
        for (field, kept) in [(&mut self.scrollback, a), (&mut self.word_chars, b), (&mut self.window_size, c)] {
            if let Some(kept) = kept {
                *field = kept;
            }
        }
    }

    fn show(&mut self, s: &Settings) {
        self.scrollback = Field::set(&s.scrollback.to_string());
        self.word_chars = Field::set(&s.word_chars);
        self.window_size = Field::set(&s.window_size.map_or(String::new(), |(c, r)| format!("{c}x{r}")));
    }

    fn field_mut(&mut self, ctl: Ctl) -> Option<&mut Field> {
        match ctl {
            Ctl::Scrollback => Some(&mut self.scrollback),
            Ctl::WordChars => Some(&mut self.word_chars),
            Ctl::WindowSize => Some(&mut self.window_size),
            _ => None,
        }
    }

    /// Move the focus, committing a text field being left.
    fn move_focus(&mut self, forward: bool) -> Outcome {
        let out = self.commit(self.focus);
        let at = ORDER.iter().position(|&c| c == self.focus).unwrap_or(0);
        let next = if forward { (at + 1) % ORDER.len() } else { (at + ORDER.len() - 1) % ORDER.len() };
        self.focus = ORDER[next];
        out
    }

    /// Validate and write the text field `ctl` holds; an invalid value is
    /// refused (shown, not written).
    fn commit(&mut self, ctl: Ctl) -> Outcome {
        self.error = None;
        match ctl {
            Ctl::Scrollback => {
                let text = self.scrollback.text.trim().to_string();
                match text.parse::<i64>() {
                    Ok(n) if (0..=MAX_SCROLLBACK).contains(&n) => {
                        self.settings.scrollback = n as usize;
                        if self.settings.scrollback.to_string() != text {
                            self.scrollback = Field::set(&n.to_string());
                        }
                        return Outcome::change("scrollback/lines", Value::Int(n));
                    }
                    _ => self.error = Some("Scrollback is a number of lines, 0 to 200000"),
                }
            }
            Ctl::WordChars => {
                let text = self.word_chars.text.clone();
                self.settings.apply("select/word_chars", &Value::Text(text.clone()));
                return Outcome::change("select/word_chars", Value::Text(self.settings.word_chars.clone()));
            }
            Ctl::WindowSize => {
                let text = self.window_size.text.trim().to_string();
                if text.is_empty() || parse_grid(&text).is_some() {
                    return Outcome::change("window/size", Value::Text(text));
                }
                self.error = Some("Window size is COLSxROWS, from 20x5 to 500x200");
            }
            _ => {}
        }
        Outcome::default()
    }

    /// Set the choice of a cycling control by `delta` entries.
    fn cycle<T: Copy + PartialEq>(options: &[T], current: T, delta: i32) -> T {
        let at = options.iter().position(|&o| o == current).unwrap_or(0) as i32;
        options[(at + delta).rem_euclid(options.len() as i32) as usize]
    }

    /// Left/Right or a click on a step: change the focused control by `delta`.
    fn step(&mut self, ctl: Ctl, delta: i32) -> Outcome {
        self.error = None;
        let s = &mut self.settings;
        match ctl {
            Ctl::Family => {
                let at = self.families.iter().position(|f| *f == s.family).unwrap_or(0) as i32;
                let next = (at + delta).clamp(0, self.families.len() as i32 - 1) as usize;
                let Some(family) = self.families.get(next).cloned() else {
                    return Outcome::default();
                };
                s.family = family.clone();
                Outcome::change("font/family", Value::Text(family))
            }
            Ctl::Size => {
                s.size = (i64::from(s.size) + i64::from(delta)).clamp(MIN_SIZE, MAX_SIZE) as u32;
                Outcome::change("font/size", Value::Int(i64::from(s.size)))
            }
            Ctl::Spacing => {
                s.line_spacing = (i64::from(s.line_spacing) + 5 * i64::from(delta)).clamp(MIN_SPACING, MAX_SPACING) as u32;
                Outcome::change("font/line_spacing", Value::Int(i64::from(s.line_spacing)))
            }
            Ctl::Scheme => {
                let at = NAMES.iter().position(|n| *n == s.scheme).unwrap_or(0) as i32;
                s.scheme = NAMES[(at + delta).rem_euclid(NAMES.len() as i32) as usize].to_string();
                Outcome::change("colors/scheme", Value::Text(s.scheme.clone()))
            }
            Ctl::CursorShape => {
                s.cursor_shape = Self::cycle(&SHAPES, s.cursor_shape, delta);
                Outcome::change("cursor/shape", Value::Text(s.cursor_shape.name().to_string()))
            }
            Ctl::Bell => {
                s.bell = Self::cycle(&BELLS, s.bell, delta);
                Outcome::change("bell", Value::Text(s.bell.name().to_string()))
            }
            Ctl::Blink | Ctl::Bold | Ctl::CopyOnSelect | Ctl::ConfirmPaste => self.toggle(ctl),
            Ctl::Scrollback | Ctl::WordChars | Ctl::WindowSize => Outcome::default(),
        }
    }

    fn toggle(&mut self, ctl: Ctl) -> Outcome {
        self.error = None;
        let s = &mut self.settings;
        let (key, flag): (&'static str, &mut bool) = match ctl {
            Ctl::Blink => ("cursor/blink", &mut s.cursor_blink),
            Ctl::Bold => ("colors/bold_is_bright", &mut s.bold_is_bright),
            Ctl::CopyOnSelect => ("select/copy_on_select", &mut s.copy_on_select),
            Ctl::ConfirmPaste => ("paste/confirm_multiline", &mut s.confirm_multiline),
            _ => return Outcome::default(),
        };
        *flag = !*flag;
        Outcome::change(key, Value::Bool(*flag))
    }

    /// Typed text: into the focused field, or toggles on Space.
    pub fn char(&mut self, c: char) -> Outcome {
        let ctl = self.focus;
        if let Some(field) = self.field_mut(ctl) {
            let digits_only = ctl == Ctl::Scrollback;
            if !digits_only || c.is_ascii_digit() {
                field.insert(c, FIELD_LIMIT);
                self.error = None;
            }
        } else if c == ' ' {
            return self.step(ctl, 1);
        }
        Outcome::default()
    }

    /// A key press (not text).
    pub fn key(&mut self, key: Key, mods: Modifiers) -> Outcome {
        if key == Key::ESCAPE {
            return Outcome { close: true, changes: Vec::new() };
        }
        if key == Key::TAB {
            return self.move_focus(!mods.shift);
        }
        let ctl = self.focus;
        if let Some(field) = self.field_mut(ctl) {
            if field.edit(key) {
                self.error = None;
                return Outcome::default();
            }
            if key == Key::RETURN {
                return self.commit(ctl);
            }
            return match key {
                Key::DOWN => self.move_focus(true),
                Key::UP => self.move_focus(false),
                _ => Outcome::default(),
            };
        }
        match key {
            Key::LEFT => self.step(ctl, -1),
            Key::RIGHT => self.step(ctl, 1),
            Key::UP if ctl == Ctl::Family => self.step(ctl, -1),
            Key::DOWN if ctl == Ctl::Family => self.step(ctl, 1),
            Key::UP => self.move_focus(false),
            Key::DOWN => self.move_focus(true),
            Key::RETURN | Key::SPACE if matches!(ctl, Ctl::Blink | Ctl::Bold | Ctl::CopyOnSelect | Ctl::ConfirmPaste) => {
                self.toggle(ctl)
            }
            _ => Outcome::default(),
        }
    }

    /// A click on `part`.
    pub fn click_part(&mut self, part: Part) -> Outcome {
        let prior = if self.field_mut(self.focus).is_some() { self.commit(self.focus) } else { Outcome::default() };
        let mut out = match part {
            Part::Family(name) => {
                self.focus = Ctl::Family;
                self.settings.family = name.clone();
                Outcome::change("font/family", Value::Text(name))
            }
            Part::SizeStep(d) => self.focus_step(Ctl::Size, d),
            Part::SpacingStep(d) => self.focus_step(Ctl::Spacing, d),
            Part::Scheme(i) => {
                self.focus = Ctl::Scheme;
                self.settings.scheme = NAMES[i.min(NAMES.len() - 1)].to_string();
                Outcome::change("colors/scheme", Value::Text(self.settings.scheme.clone()))
            }
            Part::Cursor(i) => {
                self.focus = Ctl::CursorShape;
                self.settings.cursor_shape = SHAPES[i.min(2)];
                Outcome::change("cursor/shape", Value::Text(self.settings.cursor_shape.name().to_string()))
            }
            Part::Bell(i) => {
                self.focus = Ctl::Bell;
                self.settings.bell = BELLS[i.min(3)];
                Outcome::change("bell", Value::Text(self.settings.bell.name().to_string()))
            }
            Part::Toggle(ctl) => {
                self.focus = ctl;
                self.toggle(ctl)
            }
            Part::Field(ctl) => {
                self.focus = ctl;
                Outcome::default()
            }
            Part::Close => Outcome { close: true, changes: Vec::new() },
        };
        let mut changes = prior.changes;
        changes.append(&mut out.changes);
        Outcome { close: out.close, changes }
    }

    fn focus_step(&mut self, ctl: Ctl, delta: i32) -> Outcome {
        self.focus = ctl;
        self.step(ctl, delta)
    }
}

/// A clickable part of the panel.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Part {
    Family(String),
    SizeStep(i32),
    SpacingStep(i32),
    Scheme(usize),
    Cursor(usize),
    Bell(usize),
    Toggle(Ctl),
    Field(Ctl),
    Close,
}

/// The panel's parts in window pixels, and their probe names.
pub struct PrefsLayout {
    pub panel: Rect,
    pub parts: Vec<(Part, Rect, String)>,
    /// The font preview line and the title.
    pub preview: Rect,
}

impl PrefsLayout {
    pub fn part_at(&self, x: i32, y: i32) -> Option<&Part> {
        self.parts.iter().find(|(_, r, _)| contains(*r, x, y)).map(|(p, _, _)| p)
    }
}

/// Row height and the panel's design size.
const ROW: i32 = 26;
const WIDTH: i32 = 600;
const LIST_ROWS: i32 = 9;

/// Lay the panel out centred in `window` at `scale`.
pub fn layout(prefs: &Prefs, window: (i32, i32), scale: i32) -> PrefsLayout {
    let s = scale.max(1);
    let (w, h) = (WIDTH * s, (30 + 11 * ROW + 20) * s);
    let left = ((window.0 - w) / 2).max(2 * s);
    let top = ((window.1 - h) / 2).max(2 * s);
    let panel = Rect::new(left, top, left + w, top + h);
    let mut parts: Vec<(Part, Rect, String)> = Vec::new();
    let mut add = |part: Part, rect: Rect, name: String| parts.push((part, rect, name));
    // The title row holds the close button.
    add(Part::Close, Rect::new(panel.right - 30 * s, top + 4 * s, panel.right - 6 * s, top + 26 * s), "prefs:close".into());
    // Left column: the family list and the preview line under it.
    let col = left + 10 * s;
    let list_top = top + 34 * s;
    let visible = LIST_ROWS as usize;
    let at = prefs.families.iter().position(|f| *f == prefs.settings.family).unwrap_or(0);
    let first = at.saturating_sub(visible - 1).min(prefs.families.len().saturating_sub(visible));
    for (i, name) in prefs.families.iter().skip(first).take(visible).enumerate() {
        let y = list_top + i as i32 * 22 * s;
        add(Part::Family(name.clone()), Rect::new(col, y, col + 230 * s, y + 22 * s), format!("prefs:family:{}", name.replace(' ', "_")));
    }
    let preview = Rect::new(col, list_top + LIST_ROWS * 22 * s + 8 * s, col + 270 * s, list_top + LIST_ROWS * 22 * s + 8 * s + 34 * s);
    // Right column.
    let x0 = left + 290 * s;
    let row = |i: i32| top + 34 * s + i * ROW * s;
    let control_x = x0 + 90 * s;
    let rect = |x: i32, i: i32, w: i32| Rect::new(x, row(i), x + w, row(i) + (ROW - 3) * s);
    add(Part::SizeStep(-1), rect(control_x, 0, 26 * s), "prefs:size_dec".into());
    add(Part::SizeStep(1), rect(control_x + 100 * s, 0, 26 * s), "prefs:size_inc".into());
    add(Part::SpacingStep(-1), rect(control_x, 1, 26 * s), "prefs:spacing_dec".into());
    add(Part::SpacingStep(1), rect(control_x + 100 * s, 1, 26 * s), "prefs:spacing_inc".into());
    for (i, name) in NAMES.iter().enumerate() {
        add(Part::Scheme(i), rect(x0 + i as i32 * 58 * s, 3, 54 * s), format!("prefs:scheme:{name}"));
    }
    let mut at = control_x;
    for (i, shape) in SHAPES.iter().enumerate() {
        let w = [58, 82, 52][i] * s;
        add(Part::Cursor(i), rect(at, 4, w), format!("prefs:cursor:{}", shape.name()));
        at += w + 4 * s;
    }
    add(Part::Toggle(Ctl::Blink), rect(control_x, 5, 70 * s), "prefs:blink".into());
    add(Part::Toggle(Ctl::Bold), rect(control_x + 72 * s, 5, 125 * s), "prefs:bold_is_bright".into());
    add(Part::Field(Ctl::Scrollback), rect(control_x, 6, 120 * s), "prefs:scrollback".into());
    let mut at = control_x;
    for (i, bell) in BELLS.iter().enumerate() {
        let w = [46, 54, 52, 54][i] * s;
        add(Part::Bell(i), rect(at, 7, w), format!("prefs:bell:{}", bell.name()));
        at += w + 4 * s;
    }
    add(Part::Toggle(Ctl::CopyOnSelect), rect(x0, 8, 130 * s), "prefs:copy_on_select".into());
    add(Part::Toggle(Ctl::ConfirmPaste), rect(x0 + 150 * s, 8, 148 * s), "prefs:confirm_paste".into());
    add(Part::Field(Ctl::WordChars), rect(control_x, 9, 120 * s), "prefs:word_chars".into());
    add(Part::Field(Ctl::WindowSize), rect(control_x, 10, 120 * s), "prefs:window_size".into());
    PrefsLayout { panel, parts, preview }
}

#[cfg(test)]
#[path = "prefs_tests.rs"]
mod tests;
