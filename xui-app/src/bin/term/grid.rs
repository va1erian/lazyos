//! The terminal's character grid: a UTF-8 decoder and a small ANSI/CSI
//! parser (CR/LF/BS and the cursor/erase sequences BusyBox's line editor
//! emits) feeding a fixed grid with a cursor.

/// The terminal geometry BusyBox sees through `TIOCGWINSZ` (the kernel reports
/// a fixed 80x24), so the grid matches it exactly.
pub const COLS: usize = 80;
pub const ROWS: usize = 24;

/// Decodes a byte stream into characters, holding back an incomplete UTF-8
/// sequence until the next feed so a split multi-byte character never becomes
/// two replacement characters (or a panic).
#[derive(Default)]
struct Utf8 {
    pending: Vec<u8>,
}

impl Utf8 {
    fn decode(&mut self, bytes: &[u8]) -> Vec<char> {
        let mut data = std::mem::take(&mut self.pending);
        data.extend_from_slice(bytes);
        let mut out = Vec::new();
        let mut index = 0;
        while index < data.len() {
            match std::str::from_utf8(&data[index..]) {
                Ok(text) => {
                    out.extend(text.chars());
                    index = data.len();
                }
                Err(error) => {
                    let valid = error.valid_up_to();
                    if valid > 0 {
                        // Safety: `valid_up_to` guarantees this prefix is UTF-8.
                        out.extend(
                            std::str::from_utf8(&data[index..index + valid])
                                .unwrap()
                                .chars(),
                        );
                        index += valid;
                    }
                    match error.error_len() {
                        Some(len) => {
                            out.push('\u{FFFD}');
                            index += len;
                        }
                        None => break, // incomplete tail: keep it for the next call
                    }
                }
            }
        }
        self.pending = data[index..].to_vec();
        out
    }
}

/// Where the byte-feed parser is (escape sequences can span feeds).
#[derive(Clone, Copy, PartialEq)]
enum Parse {
    Normal,
    Esc,
    Csi,
    Osc,
}

/// A fixed character grid with a cursor, fed by the child's output bytes.
pub struct Grid {
    pub cells: Vec<Vec<char>>,
    pub row: usize,
    pub col: usize,
    utf8: Utf8,
    parse: Parse,
    csi: String,
}

impl Grid {
    pub fn new() -> Grid {
        Grid {
            cells: vec![vec![' '; COLS]; ROWS],
            row: 0,
            col: 0,
            utf8: Utf8::default(),
            parse: Parse::Normal,
            csi: String::new(),
        }
    }

    /// The characters of grid row `row`, right-trimmed.
    pub fn row_text(&self, row: usize) -> String {
        let mut text: String = self.cells[row].iter().collect();
        while text.ends_with(' ') {
            text.pop();
        }
        text
    }

    /// Feed output `bytes`, returning the lines completed (a `\n` was seen).
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<String> {
        let chars = self.utf8.decode(bytes);
        let mut completed = Vec::new();
        for ch in chars {
            match self.parse {
                Parse::Normal => self.normal(ch, &mut completed),
                Parse::Esc => {
                    if ch == '[' {
                        self.parse = Parse::Csi;
                        self.csi.clear();
                    } else if ch == ']' {
                        self.parse = Parse::Osc;
                    } else {
                        self.parse = Parse::Normal;
                    }
                }
                Parse::Csi => {
                    if ch.is_ascii_digit() || ch == ';' || ch == '?' || ch == '>' {
                        if self.csi.len() < 16 {
                            self.csi.push(ch);
                        }
                    } else {
                        self.apply_csi(ch);
                        self.parse = Parse::Normal;
                    }
                }
                Parse::Osc => {
                    if ch == '\u{7}' || ch == '\u{1b}' {
                        self.parse = Parse::Normal;
                    }
                }
            }
        }
        completed
    }

    fn normal(&mut self, ch: char, completed: &mut Vec<String>) {
        match ch {
            '\r' => self.col = 0,
            '\n' => {
                let text = self.row_text(self.row);
                if !text.is_empty() {
                    completed.push(text);
                }
                self.line_feed();
            }
            '\u{8}' => self.col = self.col.saturating_sub(1),
            // Clamp to the last column so a later erase never slices past `COLS`.
            '\t' => self.col = (((self.col / 8) + 1) * 8).min(COLS),
            '\u{1b}' => self.parse = Parse::Esc,
            c if c.is_control() => {}
            c => {
                if self.col >= COLS {
                    self.col = 0;
                    self.line_feed();
                }
                self.cells[self.row][self.col] = c;
                self.col += 1;
            }
        }
    }

    fn line_feed(&mut self) {
        self.row += 1;
        if self.row >= ROWS {
            self.cells.remove(0);
            self.cells.push(vec![' '; COLS]);
            self.row = ROWS - 1;
        }
        // A real tty maps `\n` to `\r\n` (ONLCR); BusyBox relies on that, so do
        // the same here or each line walks diagonally to the right.
        self.col = 0;
    }

    /// The `index`-th `;`-separated CSI parameter, or `default` when absent.
    fn csi_param_at(&self, index: usize, default: usize) -> usize {
        self.csi
            .trim_start_matches('?')
            .split(';')
            .nth(index)
            .and_then(|value| value.parse().ok())
            .unwrap_or(default)
    }

    fn csi_param(&self, default: usize) -> usize {
        self.csi_param_at(0, default)
    }

    fn apply_csi(&mut self, final_byte: char) {
        match final_byte {
            'A' => self.row = self.row.saturating_sub(self.csi_param(1)),
            'B' => self.row = (self.row + self.csi_param(1)).min(ROWS - 1),
            'C' => self.col = (self.col + self.csi_param(1)).min(COLS - 1),
            'D' => self.col = self.col.saturating_sub(self.csi_param(1)),
            'H' | 'f' => {
                self.row = self.csi_param_at(0, 1).saturating_sub(1).min(ROWS - 1);
                self.col = self.csi_param_at(1, 1).saturating_sub(1).min(COLS - 1);
            }
            // `CSI n G` / `CSI n d`: absolute column / row (what BusyBox's
            // line editor uses to redraw the prompt and typed text).
            'G' => self.col = self.csi_param(1).saturating_sub(1).min(COLS - 1),
            'd' => self.row = self.csi_param(1).saturating_sub(1).min(ROWS - 1),
            'J' => {
                if self.csi_param(0) == 2 {
                    self.cells = vec![vec![' '; COLS]; ROWS];
                    self.row = 0;
                    self.col = 0;
                } else if self.csi_param(0) == 0 {
                    self.cells[self.row][self.col..COLS].fill(' ');
                }
            }
            'K' => {
                let start = if self.csi_param(0) == 2 { 0 } else { self.col };
                self.cells[self.row][start..COLS].fill(' ');
            }
            _ => {}
        }
    }
}

/// Whether a completed grid line is only a shell prompt: `# `, `$ `, or
/// BusyBox's default `\w \$ ` form with the working directory first (`/ # `,
/// `/tmp $ `). A prompt followed by a typed command (`/ # rhai`) is not one.
///
/// The Terminal reports exactly one `TERM:OUT` per command, so a bare prompt
/// line must never be taken for that output: the shell emits one when the
/// next command was typed before its prompt appeared (a slow reap of the
/// previous pipeline), and the real output line would then go unreported.
pub fn is_prompt(line: &str) -> bool {
    let marks = |text: &str| text.chars().all(|c| matches!(c, '#' | '$' | '>' | ' '));
    // BusyBox's `\w` is an absolute path or `~`-relative, so anything else in
    // front of the mark (`a #`, `x $`) is ordinary output that merely ends
    // like a prompt.
    let is_cwd =
        |text: &str| (text.starts_with('/') || text.starts_with('~')) && !text.contains(' ');
    match line.trim().rsplit_once(' ') {
        None => marks(line),
        Some((cwd, mark)) => is_cwd(cwd) && marks(mark),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tab_in_last_column_then_erase_does_not_panic() {
        let mut grid = Grid::new();
        grid.feed("x".repeat(COLS).as_bytes());
        grid.feed(b"\t\x1b[K\x1b[J");
        assert!(grid.col <= COLS);
    }

    #[test]
    fn cursor_position_uses_both_parameters() {
        let mut grid = Grid::new();
        grid.feed(b"\x1b[5;10H");
        assert_eq!((grid.row, grid.col), (4, 9));
    }

    /// A bare prompt, with or without the working directory, is a prompt; a
    /// prompt with a command after it, and any output line, is not.
    #[test]
    fn prompt_lines_with_a_working_directory_are_prompts() {
        for prompt in [
            "# ",
            "$",
            "> ",
            "/ #",
            "/ # ",
            "/tmp $ ",
            "/home/user $",
            "~ $",
        ] {
            assert!(is_prompt(prompt), "{prompt:?} should be a prompt");
        }
        for line in [
            "/ # rhai",
            "$ echo",
            "rhai>",
            "42",
            "a #",
            "x $",
            "cost 5 $",
            "HI",
            "rhai REPL: :help",
            "",
        ] {
            assert!(
                !is_prompt(line) || line.is_empty(),
                "{line:?} is not a prompt"
            );
        }
    }
}
