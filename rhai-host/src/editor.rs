//! A minimal line editor for the interactive REPL.
//!
//! LazyOS terminals are *raw*: the console delivers one key per `read` with no
//! echo, and the desktop Terminal writes keystrokes into a pipe. Neither has a
//! line discipline, so a program that wants to be typed at must echo and edit
//! for itself (BusyBox `sh` does the same). This is that, kept tiny and pure:
//! bytes in, echo bytes and an event out, so it is tested without a terminal.
//!
//! Supported: printable text (UTF-8), Enter (`\n`, `\r`, `\r\n`), Backspace,
//! Ctrl-U (kill line), Ctrl-C (abandon the line), Ctrl-D (end of input on an
//! empty line). Escape sequences (arrow keys, ...) are swallowed; neither
//! LazyOS terminal sends them today, and history is `!!` / `!N` in the REPL.

/// The longest line the editor accepts; further characters are dropped.
pub const MAX_LINE: usize = 1 << 20;

const BACKSPACE_ERASE: &[u8] = b"\x08 \x08";

/// What one input byte completed, if anything.
#[derive(Debug, PartialEq, Eq)]
pub enum Event {
    /// Still editing.
    Pending,
    /// Enter was pressed: the finished line (no line terminator).
    Line(String),
    /// Ctrl-C: the line (and any unfinished entry) is abandoned.
    Interrupt,
    /// Ctrl-D on an empty line.
    Eof,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Escape {
    None,
    /// After ESC.
    Esc,
    /// After `ESC [`: waiting for the final byte.
    Csi,
    /// After `ESC O` (application-mode arrows).
    Ss3,
}

/// Editing state for one line at a time.
pub struct Editor {
    line: Vec<u8>,
    escape: Escape,
    /// The previous line ended in `\r`: swallow an immediately following `\n`.
    after_cr: bool,
}

impl Default for Editor {
    fn default() -> Self {
        Self::new()
    }
}

impl Editor {
    pub fn new() -> Self {
        Self {
            line: Vec::new(),
            escape: Escape::None,
            after_cr: false,
        }
    }

    /// Feed one byte. `echo` receives what to write back to the terminal.
    pub fn feed(&mut self, byte: u8, echo: &mut Vec<u8>) -> Event {
        let was_cr = std::mem::take(&mut self.after_cr);
        if self.escape != Escape::None {
            return self.escape_byte(byte);
        }
        match byte {
            b'\n' if was_cr => Event::Pending,
            b'\r' | b'\n' => {
                self.after_cr = byte == b'\r';
                echo.push(b'\n');
                let bytes = std::mem::take(&mut self.line);
                // Bytes are only ever added as whole terminal input; a line
                // that is not UTF-8 is passed on lossily rather than dropped.
                Event::Line(String::from_utf8_lossy(&bytes).into_owned())
            }
            0x7f | 0x08 => {
                self.erase_char(echo);
                Event::Pending
            }
            0x03 => {
                self.line.clear();
                echo.extend_from_slice(b"^C\n");
                Event::Interrupt
            }
            0x04 if self.line.is_empty() => Event::Eof,
            0x15 => {
                self.erase_all(echo);
                Event::Pending
            }
            0x1b => {
                self.escape = Escape::Esc;
                Event::Pending
            }
            b if b >= 0x20 || b == b'\t' => {
                if self.line.len() < MAX_LINE {
                    self.line.push(b);
                    echo.push(b);
                }
                Event::Pending
            }
            _ => Event::Pending,
        }
    }

    fn escape_byte(&mut self, byte: u8) -> Event {
        self.escape = match (self.escape, byte) {
            (Escape::Esc, b'[') => Escape::Csi,
            (Escape::Esc, b'O') => Escape::Ss3,
            // Parameter and intermediate bytes of a CSI sequence.
            (Escape::Csi, 0x20..=0x3f) => Escape::Csi,
            // The final byte ends a CSI / SS3 sequence; a lone ESC followed
            // by anything else drops both.
            _ => Escape::None,
        };
        Event::Pending
    }

    /// Erase the last character (all bytes of a multi-byte one).
    fn erase_char(&mut self, echo: &mut Vec<u8>) {
        if self.line.is_empty() {
            return;
        }
        while let Some(byte) = self.line.pop() {
            // A UTF-8 continuation byte is 0b10xxxxxx; stop at the lead byte.
            if byte & 0xc0 != 0x80 {
                break;
            }
        }
        echo.extend_from_slice(BACKSPACE_ERASE);
    }

    fn erase_all(&mut self, echo: &mut Vec<u8>) {
        while !self.line.is_empty() {
            self.erase_char(echo);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Type `input`, returning every event that is not `Pending` plus the echo.
    fn type_bytes(editor: &mut Editor, input: &[u8]) -> (Vec<Event>, Vec<u8>) {
        let mut echo = Vec::new();
        let mut events = Vec::new();
        for &byte in input {
            match editor.feed(byte, &mut echo) {
                Event::Pending => {}
                event => events.push(event),
            }
        }
        (events, echo)
    }

    fn line(text: &str) -> Event {
        Event::Line(text.to_string())
    }

    #[test]
    fn typing_echoes_and_enter_completes_the_line() {
        let mut e = Editor::new();
        let (events, echo) = type_bytes(&mut e, b"1+2\n");
        assert_eq!(events, [line("1+2")]);
        assert_eq!(echo, b"1+2\n");
    }

    #[test]
    fn carriage_return_ends_a_line_and_crlf_is_one_line() {
        let mut e = Editor::new();
        let (events, _) = type_bytes(&mut e, b"a\rb\r\nc\n");
        assert_eq!(events, [line("a"), line("b"), line("c")]);
    }

    #[test]
    fn a_blank_line_after_cr_is_still_a_line() {
        let mut e = Editor::new();
        let (events, _) = type_bytes(&mut e, b"\r\r");
        assert_eq!(events, [line(""), line("")]);
    }

    #[test]
    fn backspace_erases_and_is_a_no_op_on_an_empty_line() {
        let mut e = Editor::new();
        let (events, echo) = type_bytes(&mut e, b"\x7f\x08ab\x7fc\n");
        assert_eq!(events, [line("ac")]);
        assert_eq!(echo, b"ab\x08 \x08c\n");
    }

    #[test]
    fn backspace_removes_a_whole_utf8_character() {
        let mut e = Editor::new();
        let (events, _) = type_bytes(&mut e, "aé€\x7f\n".as_bytes());
        assert_eq!(events, [line("aé")]);
        let (events, _) = type_bytes(&mut e, "é\x7f\x7f\n".as_bytes());
        assert_eq!(events, [line("")]);
    }

    #[test]
    fn ctrl_c_abandons_the_line_and_ctrl_d_ends_input_only_when_empty() {
        let mut e = Editor::new();
        let (events, echo) = type_bytes(&mut e, b"abc\x03");
        assert_eq!(events, [Event::Interrupt]);
        assert!(echo.ends_with(b"^C\n"));
        let (events, _) = type_bytes(&mut e, b"x\x04\n");
        assert_eq!(events, [line("x")], "Ctrl-D mid-line is ignored");
        let (events, _) = type_bytes(&mut e, b"\x04");
        assert_eq!(events, [Event::Eof]);
    }

    #[test]
    fn ctrl_u_kills_the_line() {
        let mut e = Editor::new();
        let (events, _) = type_bytes(&mut e, b"junk\x15ok\n");
        assert_eq!(events, [line("ok")]);
    }

    #[test]
    fn unknown_escape_sequences_are_swallowed() {
        let mut e = Editor::new();
        let (events, echo) = type_bytes(&mut e, b"a\x1b[1;5Cb\x1b[3~c\x1bxd\n");
        assert_eq!(events, [line("abcd")]);
        assert_eq!(echo, b"abcd\n");
    }

    #[test]
    fn other_control_bytes_are_ignored_and_tab_is_kept() {
        let mut e = Editor::new();
        let (events, _) = type_bytes(&mut e, b"a\x01\x02\tb\x00\n");
        assert_eq!(events, [line("a\tb")]);
    }

    #[test]
    fn an_over_long_line_stops_growing() {
        let mut e = Editor::new();
        let mut echo = Vec::new();
        for _ in 0..MAX_LINE + 100 {
            e.feed(b'x', &mut echo);
        }
        assert_eq!(echo.len(), MAX_LINE);
        match e.feed(b'\n', &mut echo) {
            Event::Line(text) => assert_eq!(text.len(), MAX_LINE),
            other => panic!("{other:?}"),
        }
    }
}
