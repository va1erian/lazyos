//! `xui-term`: the desktop Terminal (issue #216), a `xuid` client hosting the
//! LazyOS shell.
//!
//! A desktop session has no console: `xuid` owns the display grant, so the
//! kernel mux (where the native `sh` and `logind`'s prompt live) stops
//! painting. The Terminal is a windowed client instead. It runs the shell's
//! language and command layer in-process (`lazyos-lang`, the same crate as the
//! native `SH.ELF`), so `help`, `cat`, `let x = 6*7`, `x*2` and friends behave
//! exactly as at the console. There is no pty in the kernel yet, so the
//! interpreter is linked in rather than spawned; a real terminal emulator over
//! pipes (BusyBox `sh`) is the follow-up when a pty exists.
//!
//! Serial evidence: `TERM:UP:PASS` after the first frame, `TERM:CMD:<line>`
//! and `TERM:OUT:<first output line>` for each command run, `TERM:EXIT:PASS`
//! when the shell exits or the window is closed.

use std::rc::Rc;

use std::io::Read;

use lazyos_lang::repl::{Flow, Shell, BANNER, CAT_LIMIT};
use xui_app::backend::LazyOSBackend;
use xui_core::app::{run_app, App, Ui};
use xui_core::backend::{Backend, Event, NodeKind, NodeSpec, PlatformSpec};
use xui_core::{Canvas, Color, Control, Dip, Key, Rect, TextStyle};

use std::cell::RefCell;

/// Window size when a compositor lays the app out; as the display owner the
/// terminal fills the screen instead.
const WINDOW: (i32, i32) = (640, 400);
/// Text size and the monospace cell it implies (JetBrains Mono is 0.6 em wide).
const FONT: f32 = 15.0;
const CELL_W: f32 = FONT * 0.6;
const LINE_H: i32 = 20;
const PAD: i32 = 10;
/// Output lines kept for scrollback.
const SCROLLBACK: usize = 400;
/// Commands remembered for Up/Down.
const HISTORY: usize = 50;
const PROMPT: &str = "> ";

const BG: Color = Color::rgb(0x16, 0x18, 0x1d);
const FG: Color = Color::rgb(0xd7, 0xdb, 0xe0);
const PROMPT_FG: Color = Color::rgb(0x6c, 0xb6, 0xff);

/// One application message.
enum Msg {
    Char(char),
    History(i32),
    Close,
}

/// The terminal's text state, shared with the painter.
struct Screen {
    /// Completed and in-progress output lines (the last is still open).
    lines: Vec<String>,
    /// The command being typed.
    input: String,
    history: Vec<String>,
    /// Position in `history` while browsing with Up/Down.
    browsing: Option<usize>,
}

impl Screen {
    fn new() -> Screen {
        let mut screen = Screen {
            lines: vec![String::new()],
            input: String::new(),
            history: Vec::new(),
            browsing: None,
        };
        screen.print(BANNER);
        screen
    }

    /// Append shell output, splitting on newlines and trimming scrollback.
    fn print(&mut self, text: &str) {
        for piece in text.split_inclusive('\n') {
            if let Some(last) = self.lines.last_mut() {
                last.push_str(piece.trim_end_matches('\n'));
            }
            if piece.ends_with('\n') {
                self.lines.push(String::new());
            }
        }
        let excess = self.lines.len().saturating_sub(SCROLLBACK);
        self.lines.drain(..excess);
    }

    /// Wrap every line to `cols` columns; the open line is the prompt row.
    fn rows(&self, cols: usize) -> Vec<(bool, String)> {
        let mut rows = Vec::new();
        let last = self.lines.len() - 1;
        for (index, line) in self.lines.iter().enumerate() {
            let text = if index == last {
                // The open line ends with the prompt and the typed command.
                format!("{line}{PROMPT}{}_", self.input)
            } else {
                line.clone()
            };
            let chars: Vec<char> = text.chars().collect();
            if chars.is_empty() {
                rows.push((false, String::new()));
            }
            for (part, chunk) in chars.chunks(cols.max(1)).enumerate() {
                let prompt = index == last && part == 0 && chunk.starts_with(&['>', ' ']);
                rows.push((prompt, chunk.iter().collect()));
            }
        }
        rows
    }
}

/// The app: the shell plus the screen it prints on.
struct Terminal {
    shell: Shell,
    screen: Rc<RefCell<Screen>>,
    root: Control<Msg>,
}

impl Terminal {
    /// Run the typed line through the shell and print its output.
    fn submit(&mut self, ui: &mut Ui<Msg>) {
        let line = std::mem::take(&mut self.screen.borrow_mut().input);
        let mut screen = self.screen.borrow_mut();
        screen.browsing = None;
        screen.print(&format!("{PROMPT}{line}\n"));
        if !line.trim().is_empty() {
            screen.history.push(line.clone());
            let excess = screen.history.len().saturating_sub(HISTORY);
            screen.history.drain(..excess);
        }
        let mut output = String::new();
        let flow = self
            .shell
            .exec_line(&line, &mut read_file, &mut |chunk| output.push_str(chunk));
        screen.print(&output);
        drop(screen);
        if !line.trim().is_empty() {
            println!("TERM:CMD:{}", line.trim());
            println!("TERM:OUT:{}", output.lines().next().unwrap_or(""));
        }
        if flow == Flow::Exit {
            println!("TERM:EXIT:PASS");
            ui.quit();
        }
    }

    /// Recall an earlier command (`step` -1 goes back, +1 forward).
    fn browse(&mut self, step: i32) {
        let mut screen = self.screen.borrow_mut();
        let count = screen.history.len();
        if count == 0 {
            return;
        }
        let next = match (screen.browsing, step) {
            (None, s) if s < 0 => Some(count - 1),
            (Some(at), s) if s < 0 => Some(at.saturating_sub(1)),
            (Some(at), _) if at + 1 < count => Some(at + 1),
            _ => None,
        };
        screen.browsing = next;
        screen.input = next
            .map(|at| screen.history[at].clone())
            .unwrap_or_default();
    }
}

impl App for Terminal {
    type Msg = Msg;

    fn update(&mut self, msg: Msg, ui: &mut Ui<Msg>) {
        match msg {
            Msg::Char('\n') => self.submit(ui),
            Msg::Char('\u{8}') => {
                self.screen.borrow_mut().input.pop();
            }
            Msg::Char(ch) if !ch.is_control() => self.screen.borrow_mut().input.push(ch),
            Msg::Char(_) => {}
            Msg::History(step) => self.browse(step),
            Msg::Close => {
                println!("TERM:EXIT:PASS");
                ui.quit();
            }
        }
        ui.invalidate(self.root.id());
    }
}

/// `cat`'s file source: the boot volume through the Linux ABI.
fn read_file(name: &str) -> Option<Vec<u8>> {
    // Only `CAT_LIMIT` bytes are ever shown, so never read more than that.
    let read = |path: &str| {
        let mut bytes = Vec::new();
        std::fs::File::open(path)
            .and_then(|file| file.take(CAT_LIMIT as u64).read_to_end(&mut bytes))
            .map(|_| bytes)
            .ok()
    };
    read(name).or_else(|| read(&format!("/{name}")))
}

fn main() {
    let backend = match LazyOSBackend::connect() {
        Ok(backend) => Rc::new(backend),
        Err(code) => {
            println!("TERM:BIND:FAIL:{code}");
            std::process::exit(1);
        }
    };
    let (width, height) = backend.window_size(WINDOW);
    backend.on_first_frame(|| println!("TERM:UP:PASS"));

    let screen = Rc::new(RefCell::new(Screen::new()));
    let spec = PlatformSpec::new("Terminal").size(Dip(width as f32), Dip(height as f32));
    let outcome = run_app(Rc::clone(&backend) as Rc<dyn Backend>, spec, |ui| {
        let root = Control::new(ui, &NodeSpec::new(NodeKind::Custom, ui.client_rect()))
            .expect("root node");
        {
            let screen = Rc::clone(&screen);
            root.set_painter(Rc::new(move |canvas| paint(canvas, &screen.borrow())));
        }
        root.on_events(|event| match event {
            Event::Char(ch) => Some(Msg::Char(*ch)),
            Event::KeyDown { key, .. } if *key == Key::UP => Some(Msg::History(-1)),
            Event::KeyDown { key, .. } if *key == Key::DOWN => Some(Msg::History(1)),
            _ => None,
        });
        root.focus();
        ui.on_close(|| Some(Msg::Close));
        Terminal {
            shell: Shell::new(),
            screen: Rc::clone(&screen),
            root,
        }
    });

    backend.unbind();
    match outcome {
        Ok(()) => std::process::exit(0),
        Err(error) => {
            println!("TERM:RUN:FAIL:{error}");
            std::process::exit(1);
        }
    }
}

/// Paint the bottom of the scrollback that fits the window.
fn paint(canvas: &mut dyn Canvas, screen: &Screen) {
    let bounds = canvas.bounds();
    canvas.clear(BG);
    let cols = ((bounds.width() - 2 * PAD) as f32 / CELL_W) as usize;
    let visible = ((bounds.height() - 2 * PAD) / LINE_H).max(1) as usize;
    let rows = screen.rows(cols);
    let first = rows.len().saturating_sub(visible);
    for (slot, (is_prompt, text)) in rows[first..].iter().enumerate() {
        let top = bounds.top + PAD + slot as i32 * LINE_H;
        let color = if *is_prompt { PROMPT_FG } else { FG };
        let rect = Rect::new(bounds.left + PAD, top, bounds.right - PAD, top + LINE_H);
        canvas.draw_text(text, rect, &TextStyle::new(color, Dip(FONT)).middle());
    }
}
