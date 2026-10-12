//! Escape sequences to state changes: the `vte` parser calls these, and each
//! decodes its parameters (bounded by the parser) into one `State` operation.
//! What is not listed is ignored, never an error: a program's output is
//! untrusted input.

use vte::{Params, Perform};

use crate::cell::{Color, Flags};
use crate::input::MouseMode;
use crate::state::State;

/// The `index`-th parameter's first value, if present.
fn nth(params: &Params, index: usize) -> Option<u16> {
    params.iter().nth(index).and_then(|group| group.first().copied())
}

/// A count parameter: absent or `0` mean `default`.
fn arg(params: &Params, index: usize, default: usize) -> usize {
    match nth(params, index) {
        None | Some(0) => default,
        Some(value) => usize::from(value),
    }
}

/// A selector parameter, where `0` is a real value.
fn mode(params: &Params) -> usize {
    nth(params, 0).map_or(0, usize::from)
}

impl Perform for State {
    fn print(&mut self, c: char) {
        State::print(self, c);
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            0x08 => self.move_to(self.row, self.col.saturating_sub(1)),
            0x09 => self.tab_forward(1),
            0x0a..=0x0c => self.line_feed(),
            0x0d => self.move_to(self.row, 0),
            _ => {}
        }
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: char) {
        if ignore {
            return;
        }
        match intermediates {
            [] => self.csi(params, action),
            [b'?'] => self.private_mode(params, action),
            _ => {}
        }
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], ignore: bool, byte: u8) {
        if ignore {
            return;
        }
        match (intermediates, byte) {
            ([], b'D') => self.index(),
            ([], b'E') => {
                self.index();
                self.move_to(self.row, 0);
            }
            ([], b'M') => self.reverse_index(),
            ([], b'7') => self.save_cursor(),
            ([], b'8') => self.restore_cursor(),
            ([], b'H') => self.tabs[self.col] = true,
            ([], b'c') => self.reset(),
            ([b'#'], b'8') => self.alignment_pattern(),
            _ => {}
        }
    }
}

impl State {
    fn csi(&mut self, params: &Params, action: char) {
        let n = arg(params, 0, 1);
        match action {
            'A' => self.move_up(n),
            'B' | 'e' => self.move_down(n),
            'C' | 'a' => self.move_to(self.row, self.col.saturating_add(n)),
            'D' => self.move_to(self.row, self.col.saturating_sub(n)),
            'E' => {
                self.move_down(n);
                self.move_to(self.row, 0);
            }
            'F' => {
                self.move_up(n);
                self.move_to(self.row, 0);
            }
            'G' | '`' => self.move_to(self.row, n - 1),
            'd' => self.move_to(n - 1, self.col),
            'H' | 'f' => self.move_to(n - 1, arg(params, 1, 1) - 1),
            'I' => self.tab_forward(n),
            'Z' => self.tab_back(n),
            'g' => match mode(params) {
                0 => self.tabs[self.col] = false,
                3 => self.tabs.fill(false),
                _ => {}
            },
            'J' => self.erase_display(mode(params)),
            'K' => self.erase_line(mode(params)),
            'L' => self.insert_lines(n),
            'M' => self.delete_lines(n),
            '@' => self.insert_chars(n),
            'P' => self.delete_chars(n),
            'X' => self.erase_chars(n),
            'S' => self.scroll_up(n),
            'T' => self.scroll_down(n),
            'r' => {
                let bottom = arg(params, 1, self.height());
                self.set_region(n - 1, bottom - 1);
            }
            's' => self.save_cursor(),
            'u' => self.restore_cursor(),
            'm' => self.sgr(params),
            'h' | 'l' => {
                // LNM: a line feed also returns the carriage.
                if nth(params, 0) == Some(20) {
                    self.newline = action == 'h';
                }
            }
            'n' => self.status_report(mode(params)),
            'c' if mode(params) == 0 => self.reply.extend_from_slice(b"\x1b[?6c"),
            _ => {}
        }
    }

    /// DSR: `5` asks if the terminal is fine, `6` where the cursor is.
    fn status_report(&mut self, which: usize) {
        match which {
            5 => self.reply.extend_from_slice(b"\x1b[0n"),
            6 => {
                let report = format!("\x1b[{};{}R", self.row + 1, self.col + 1);
                self.reply.extend_from_slice(report.as_bytes());
            }
            _ => {}
        }
    }

    /// `CSI ? Pm h` / `l`: DEC private modes.
    fn private_mode(&mut self, params: &Params, action: char) {
        if !matches!(action, 'h' | 'l') {
            return;
        }
        let on = action == 'h';
        for group in params.iter() {
            match group.first() {
                Some(7) => self.autowrap = on,
                Some(25) => self.cursor_visible = on,
                Some(code @ (1000 | 1002 | 1003)) => {
                    let level = MouseMode::from_code(*code);
                    if on {
                        self.mouse = level;
                    } else if self.mouse == level {
                        self.mouse = MouseMode::Off;
                    }
                }
                Some(1006) => self.mouse_sgr = on,
                Some(2004) => self.bracketed_paste = on,
                Some(47 | 1047 | 1049) => self.set_alternate(on),
                _ => {}
            }
        }
    }

    /// SGR: colours and attributes, with both the `;` and `:` forms of the
    /// extended colours.
    fn sgr(&mut self, params: &Params) {
        let mut groups = params.iter().peekable();
        if groups.peek().is_none() {
            self.attr = Default::default();
        }
        while let Some(group) = groups.next() {
            let code = group.first().copied().unwrap_or(0);
            let flags = &mut self.attr.flags;
            match code {
                0 => self.attr = Default::default(),
                1 => flags.set(Flags::BOLD, true),
                2 => flags.set(Flags::DIM, true),
                3 => flags.set(Flags::ITALIC, true),
                4 => flags.set(Flags::UNDERLINE, group.get(1) != Some(&0)),
                7 => flags.set(Flags::INVERSE, true),
                8 => flags.set(Flags::HIDDEN, true),
                9 => flags.set(Flags::STRIKE, true),
                21 => flags.set(Flags::UNDERLINE, true),
                22 => {
                    flags.set(Flags::BOLD, false);
                    flags.set(Flags::DIM, false);
                }
                23 => flags.set(Flags::ITALIC, false),
                24 => flags.set(Flags::UNDERLINE, false),
                27 => flags.set(Flags::INVERSE, false),
                28 => flags.set(Flags::HIDDEN, false),
                29 => flags.set(Flags::STRIKE, false),
                30..=37 => self.attr.fg = Color::Indexed((code - 30) as u8),
                39 => self.attr.fg = Color::Default,
                40..=47 => self.attr.bg = Color::Indexed((code - 40) as u8),
                49 => self.attr.bg = Color::Default,
                90..=97 => self.attr.fg = Color::Indexed((code - 90 + 8) as u8),
                100..=107 => self.attr.bg = Color::Indexed((code - 100 + 8) as u8),
                38 | 48 => {
                    let color = extended_color(group, &mut groups);
                    if let Some(color) = color {
                        if code == 38 {
                            self.attr.fg = color;
                        } else {
                            self.attr.bg = color;
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

/// The colour after `38`/`48`: `group` holds the `:` sub-parameters
/// (`38:2::r:g:b`, `38:5:n`), or the next groups hold them (`38;2;r;g;b`).
fn extended_color<'a>(
    group: &[u16],
    rest: &mut std::iter::Peekable<impl Iterator<Item = &'a [u16]>>,
) -> Option<Color> {
    let mut next = |at: usize| -> Option<u16> {
        if group.len() > 1 {
            group.get(at).copied()
        } else {
            rest.next().and_then(|g| g.first().copied())
        }
    };
    match next(1)? {
        5 => Some(Color::Indexed(next(2)? as u8)),
        2 => {
            // The colon form may carry an empty colour-space slot: `2::r:g:b`.
            let colon = group.len() > 1;
            let skip = usize::from(colon && group.len() >= 6);
            let (r, g, b) = (next(2 + skip)?, next(3 + skip)?, next(4 + skip)?);
            Some(Color::Rgb(r as u8, g as u8, b as u8))
        }
        _ => None,
    }
}
