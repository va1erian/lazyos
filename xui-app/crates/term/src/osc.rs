//! Operating system commands: the colour ones (4, 10, 11, 12 set and query;
//! 104, 110, 111, 112 reset) and the dispatch of the rest (`osc_meta.rs`).
//! Every parameter is bounded: the text comes from the program's output.

use crate::palette::{parse_color, Rgb};
use crate::state::State;

/// Most colour pairs one OSC 4 is allowed to carry.
const MAX_PAIRS: usize = 32;

impl State {
    pub(crate) fn osc(&mut self, params: &[&[u8]], bell_terminated: bool) {
        let Some(command) = params.first().and_then(|p| std::str::from_utf8(p).ok()) else {
            return;
        };
        let terminator = if bell_terminated { "\x07" } else { "\x1b\\" };
        let text = |at: usize| params.get(at).and_then(|p| std::str::from_utf8(p).ok());
        match command {
            "4" => {
                for pair in params[1..].chunks(2).take(MAX_PAIRS) {
                    let [index, spec] = pair else { break };
                    let index = std::str::from_utf8(index)
                        .ok()
                        .and_then(|n| n.parse::<u8>().ok());
                    let spec = std::str::from_utf8(spec).ok();
                    if let (Some(index), Some(spec)) = (index, spec) {
                        self.color_command(Target::Index(index), spec, terminator);
                    }
                }
            }
            "10" | "11" | "12" => {
                let target = match command {
                    "10" => Target::Fg,
                    "11" => Target::Bg,
                    _ => Target::Cursor,
                };
                if let Some(spec) = text(1) {
                    self.color_command(target, spec, terminator);
                }
            }
            "104" => {
                if params.len() == 1 {
                    self.palette.reset();
                }
                for index in params[1..].iter().take(MAX_PAIRS) {
                    let index = std::str::from_utf8(index)
                        .ok()
                        .and_then(|n| n.parse::<u8>().ok());
                    if let Some(index) = index {
                        self.palette.reset_index(index);
                    }
                }
            }
            // Titles (0 also names the icon, 1 only the icon: ignored), the
            // working directory and hyperlinks: `osc_meta.rs`.
            "0" | "2" => self.set_title(params),
            "7" => self.set_cwd(params),
            "8" => self.set_link(params),
            "110" => self.palette.set_fg(self.palette.scheme().fg),
            "111" => self.palette.set_bg(self.palette.scheme().bg),
            "112" => self.palette.set_cursor(self.palette.scheme().cursor),
            _ => {}
        }
    }

    /// Set (`spec` a colour) or query (`spec` is `?`) one colour.
    fn color_command(&mut self, target: Target, spec: &str, terminator: &str) {
        if spec == "?" {
            let color = match target {
                Target::Index(i) => self.palette.get(i),
                Target::Fg => self.palette.fg(),
                Target::Bg => self.palette.bg(),
                Target::Cursor => self.palette.cursor(),
            };
            let head = match target {
                Target::Index(i) => format!("4;{i}"),
                Target::Fg => "10".to_string(),
                Target::Bg => "11".to_string(),
                Target::Cursor => "12".to_string(),
            };
            let reply = format!("\x1b]{head};rgb:{}{terminator}", color.spec16());
            self.reply.extend_from_slice(reply.as_bytes());
        } else if let Some(color) = parse_color(spec) {
            self.set_color(target, color);
        }
    }

    fn set_color(&mut self, target: Target, color: Rgb) {
        match target {
            Target::Index(i) => self.palette.set(i, color),
            Target::Fg => self.palette.set_fg(color),
            Target::Bg => self.palette.set_bg(color),
            Target::Cursor => self.palette.set_cursor(color),
        }
    }
}

#[derive(Clone, Copy)]
enum Target {
    Index(u8),
    Fg,
    Bg,
    Cursor,
}
