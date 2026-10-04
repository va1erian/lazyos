//! `xui-term`: the desktop Terminal (issues #216, #254), a `xuid` client that
//! hosts BusyBox `sh` as a real child process.
//!
//! A desktop session has no console: `xuid` owns the display grant, so the
//! kernel mux (where `logind`'s prompt lives) stops painting. The Terminal is a
//! windowed front end instead. It opens a pseudo-terminal (`term/pty.rs`),
//! starts `busybox sh -i` on its slave as the session's controlling terminal,
//! writes keystrokes to the master and parses what the master reads into a
//! character grid (a small VT100 subset, `term/grid.rs`). The event loop
//! parks on the master beside the compositor endpoints (`watch_fd`), so the
//! UI thread drains it the moment the shell writes, and repaints only the
//! rows that changed (`term/view.rs`).
//!
//! The shell is a real, out-of-process BusyBox on a real tty: pipes,
//! redirection and job control work as at a Linux terminal, `^C` is the line
//! discipline's `SIGINT` for the foreground job, programs that read cooked
//! input (`cat`, `dash`, `lua`) get echo and line editing from the kernel,
//! and full-screen programs (`vi`, `less`) get raw mode and the window size.
//!
//! Serial evidence: `TERM:UP:PASS` after the first frame, `TERM:CMD:<line>` for
//! each submitted command, `TERM:OUT:<line>` for each completed output line,
//! `TERM:EXIT:PASS` when the shell exits or the window is closed, and
//! `TERM:SPAWN:FAIL:<reason>` when the child cannot start.

use std::io::{Read, Write};
use std::os::unix::io::RawFd;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command};

use xui_app::backend::LazyOSBackend;
use xui_app::themed::run_themed;
use xui_core::app::{App, Ui};
use xui_core::backend::{Event, NodeKind, NodeSpec, PlatformSpec};
use xui_core::{Canvas, Color, Control, Dip, Key, Rect, TextStyle};

#[path = "term/grid.rs"]
mod grid;
#[path = "term/perf.rs"]
mod perf;
#[path = "term/pty.rs"]
mod pty;
#[path = "term/view.rs"]
mod view;
use grid::{is_prompt, Grid, COLS, ROWS};
use perf::{PaintClock, Perf};
use view::{first_row, intersect, overlaps, Layout, Shown};

use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// The shell to host: the shipped `/system/bin/busybox`, started as
/// `busybox sh -i`. `argv[0]` (`fhs::boot::BUSYBOX_ARGV0`) is what BusyBox
/// dispatches on, so it is set explicitly.
const SHELL: &str = fhs::bin::BUSYBOX;
/// Window size when a compositor lays the app out; as the display owner the
/// terminal fills the screen instead.
const WINDOW: (i32, i32) = (640, 400);
/// Text size and the monospace cell it implies (JetBrains Mono is 0.6 em wide).
const FONT: f32 = 15.0;
const CELL_W: f32 = FONT * 0.6;
const LINE_H: i32 = 20;
const PAD: i32 = 10;
/// A safety-net drain: the pty wakes the loop itself (`watch_fd`), so this
/// only matters if a wake were ever missed. The drain only repaints when
/// bytes actually arrived, so an idle terminal costs no frames.
const FALLBACK_MILLIS: u32 = 1000;

/// Background, text and cursor colours for the desktop's dark and light modes.
struct Palette {
    bg: Color,
    fg: Color,
    cursor: Color,
}

/// Midnight's navy, a shade deeper than a window so the grid reads as a well.
const DARK: Palette = Palette {
    bg: Color::rgb(0x15, 0x19, 0x28),
    fg: Color::rgb(0xdc, 0xe1, 0xf0),
    cursor: Color::rgb(0x5f, 0xd0, 0x8f),
};
const LIGHT: Palette = Palette {
    bg: Color::rgb(0xfb, 0xfb, 0xfb),
    fg: Color::rgb(0x1f, 0x23, 0x28),
    cursor: Color::rgb(0x09, 0x69, 0xda),
};

/// One application message.
enum Msg {
    /// The poll timer fired: drain output, maybe finish.
    Tick,
    /// A translated character from the keyboard.
    Char(char),
    /// A key press, and whether Ctrl was held: arrows, Escape and the like,
    /// and Ctrl+letter chords (which arrive without a `Char`). Enter and
    /// Backspace also arrive as `Char`s, which is the path that handles them.
    Key(Key, bool),
    Close,
}

/// Put a descriptor into non-blocking mode so the poll timer's read never
/// stalls the UI thread.
fn set_nonblocking(fd: RawFd) {
    // Safety: `fcntl(F_SETFL)` on a descriptor this process owns; it only
    // changes the open-file flags and cannot fail in a way that corrupts memory.
    unsafe { libc::fcntl(fd, libc::F_SETFL, libc::O_NONBLOCK) };
}

/// The app: the child shell, the pty master, and the grid it paints on.
struct Terminal {
    child: Child,
    master: pty::Master,
    /// The shell side is gone (the master read `EIO` or end-of-file).
    hung_up: bool,
    grid: Rc<RefCell<Grid>>,
    typed: String,
    /// The last submitted command and whether its first output line is still
    /// expected, so exactly one `TERM:OUT` is printed per command (the shell
    /// echo and the bare prompt are skipped).
    last_cmd: Option<String>,
    awaiting_output: bool,
    root: Control<Msg>,
    /// What is on screen, so a drain repaints only the rows that changed;
    /// the painter's latest layout feeds it.
    shown: Shown,
    layout: Rc<Cell<Option<Layout>>>,
    /// `TERM:PERF` evidence (`term/perf.rs`).
    perf: Perf,
    paint_clock: Rc<PaintClock>,
}

impl Terminal {
    fn send(&mut self, bytes: &[u8]) {
        let _ = self.master.file().write_all(bytes);
    }

    /// Drain the master without blocking; return the bytes read.
    fn drain(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        let mut chunk = [0u8; 4096];
        while !self.hung_up {
            match self.master.file().read(&mut chunk) {
                Ok(0) => self.hung_up = true,
                Ok(n) => out.extend_from_slice(&chunk[..n]),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                // `EIO`: every slave descriptor closed, the shell is gone.
                Err(_) => self.hung_up = true,
            }
        }
        out
    }

    /// Forward one character to the child, keeping a local echo of the line so
    /// `TERM:CMD` can report the submitted command.
    fn key(&mut self, ch: char) {
        match ch {
            '\n' | '\r' => {
                let line = self.typed.trim().to_string();
                self.typed.clear();
                self.send(b"\r");
                if !line.is_empty() {
                    self.perf.submitted(&self.paint_clock);
                    println!("TERM:CMD:{line}");
                    self.last_cmd = Some(line);
                    self.awaiting_output = true;
                }
            }
            '\u{8}' => {
                self.typed.pop();
                self.send(b"\x7f");
            }
            // Control characters (^C, ^D, ^Z, ^U, ...) go to the line
            // discipline, which turns them into signals or edits.
            '\t' => self.send(b"\t"),
            // Escape arrives through the key path (`key_sequence`).
            '\u{1b}' => {}
            c if c.is_control() => self.send(&[c as u8]),
            c => {
                self.typed.push(c);
                self.perf.sent_key();
                self.send(c.to_string().as_bytes());
            }
        }
    }

    fn finish(&self, ui: &mut Ui<Msg>) {
        println!("TERM:EXIT:PASS");
        ui.quit();
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl App for Terminal {
    type Msg = Msg;

    fn update(&mut self, msg: Msg, ui: &mut Ui<Msg>) {
        match msg {
            Msg::Tick => {
                let bytes = self.drain();
                if !bytes.is_empty() {
                    let completed = self.grid.borrow_mut().feed(&bytes);
                    for line in completed {
                        let marker = line.trim();
                        if marker.is_empty() || is_prompt(marker) {
                            continue;
                        }
                        // Skip the shell's echo of the command it is about to
                        // run; the next non-empty line is the output.
                        if let Some(command) = &self.last_cmd {
                            if self.awaiting_output && marker.ends_with(command.as_str()) {
                                continue;
                            }
                        }
                        if self.awaiting_output {
                            self.awaiting_output = false;
                            println!("TERM:OUT:{marker}");
                        }
                    }
                    let changed = self.shown.update(&self.grid.borrow(), self.layout.get());
                    if let Some(rect) = changed {
                        ui.invalidate_rect(self.root.id(), rect);
                    }
                    let at_prompt = {
                        let grid = self.grid.borrow();
                        let row = grid.row_text(grid.row);
                        !row.is_empty() && is_prompt(&row)
                    };
                    self.perf.read(bytes.len(), at_prompt, &self.paint_clock);
                }
                if self.hung_up {
                    self.finish(ui);
                }
            }
            Msg::Char(ch) => self.key(ch),
            // Both input paths also deliver Enter and Backspace as `Char`s:
            // handling the key too would send each one twice.
            Msg::Key(Key::RETURN | Key::BACK, _) => {}
            Msg::Key(key, true) => {
                // Ctrl+A..Z: the control byte (^C, ^D, ^Z ...), which the
                // line discipline turns into a signal or an edit.
                if let Some(byte) = control_byte(key) {
                    self.send(&[byte]);
                }
            }
            Msg::Key(key, false) => {
                if let Some(sequence) = key_sequence(key) {
                    self.send(sequence);
                }
            }
            Msg::Close => self.finish(ui),
        }
    }
}

fn main() {
    std::panic::set_hook(Box::new(|info| println!("TERM:PANIC:{info}")));
    // The Terminal is a fixed character grid, so it alone uses the monospace
    // face; this must precede the backend, which builds the shaper lazily.
    xui_canvas::add_font(xui_app::font::MONO_BYTES.to_vec());
    xui_canvas::set_default_family(xui_app::font::MONO_FAMILY);
    let backend = match LazyOSBackend::connect() {
        Ok(backend) => Rc::new(backend),
        Err(code) => {
            println!("TERM:BIND:FAIL:{code}");
            std::process::exit(1);
        }
    };
    let (width, height) = backend.window_size(WINDOW);
    backend.on_first_frame(|| println!("TERM:UP:PASS"));

    let mut shell = Command::new(SHELL);
    shell.arg0(fhs::boot::BUSYBOX_ARGV0).args(["sh", "-i"]);
    shell.env("TERM", "vt100");
    // The rows programs see are the ones the window shows (the paint
    // formula), so a full-screen program's status line stays visible. The
    // width stays the grid's full 80 columns: a narrower tty makes the
    // shell wrap a long command line onto two rows, and `TERM:OUT` (one per
    // command, the line after the command's own echo) then reports the
    // wrapped tail of the echo instead of the output (the Doom session's
    // 75-column timedemo command). Columns past the window are clipped, as
    // they always were.
    let rows = (((height as i32 - 2 * PAD) / LINE_H).max(1) as usize).min(ROWS);
    let cols = COLS;
    let (master, child) = match pty::spawn(shell, rows as u16, cols as u16) {
        Ok(spawned) => spawned,
        Err(error) => {
            println!("TERM:SPAWN:FAIL:{error}");
            backend.unbind();
            std::process::exit(1);
        }
    };
    set_nonblocking(master.fd());

    let grid = Rc::new(RefCell::new(Grid::new()));
    let paint_clock = Rc::new(PaintClock::default());
    let layout = Rc::new(Cell::new(None));
    backend.watch_fd(master.fd());
    let spec = PlatformSpec::new("Terminal").size(Dip(width as f32), Dip(height as f32));
    let outcome = run_themed(&backend, spec, |ui| {
        let root = Control::new(ui, &NodeSpec::new(NodeKind::Custom, ui.client_rect()))
            .expect("root node");
        {
            let grid = Rc::clone(&grid);
            let theme = ui.theme_handle();
            let clock = Rc::clone(&paint_clock);
            let layout = Rc::clone(&layout);
            let backend = Rc::clone(&backend);
            root.set_painter(Rc::new(move |canvas| {
                let palette = if theme.get().is_dark { &DARK } else { &LIGHT };
                let damage = backend.paint_damage();
                let laid = clock.time(|| paint(canvas, palette, &grid.borrow(), damage));
                layout.set(Some(laid));
            }));
        }
        root.on_events(|event| match event {
            Event::Char(ch) => Some(Msg::Char(*ch)),
            Event::KeyDown { key, modifiers, .. } => Some(Msg::Key(*key, modifiers.ctrl)),
            _ => None,
        });
        root.focus();
        ui.on_close(|| Some(Msg::Close));
        ui.on_timer(|_| Some(Msg::Tick));
        ui.set_timer(FALLBACK_MILLIS);
        Terminal {
            child,
            master,
            hung_up: false,
            grid: Rc::clone(&grid),
            typed: String::new(),
            last_cmd: None,
            awaiting_output: false,
            root,
            shown: Shown::default(),
            layout: Rc::clone(&layout),
            perf: Perf::default(),
            paint_clock: Rc::clone(&paint_clock),
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

/// The control byte of a Ctrl+letter chord (`Ctrl+C` is 3), or `None`.
fn control_byte(key: Key) -> Option<u8> {
    let code = key.code();
    (u16::from(b'A')..=u16::from(b'Z'))
        .contains(&code)
        .then(|| code as u8 - b'A' + 1)
}

/// The bytes a VT100 sends for a non-text key (`None`: nothing to send).
fn key_sequence(key: Key) -> Option<&'static [u8]> {
    Some(match key {
        Key::ESCAPE => b"\x1b",
        Key::UP => b"\x1b[A",
        Key::DOWN => b"\x1b[B",
        Key::RIGHT => b"\x1b[C",
        Key::LEFT => b"\x1b[D",
        Key::HOME => b"\x1b[H",
        Key::END => b"\x1b[F",
        Key::INSERT => b"\x1b[2~",
        Key::DELETE => b"\x1b[3~",
        Key::PAGE_UP => b"\x1b[5~",
        Key::PAGE_DOWN => b"\x1b[6~",
        _ => return None,
    })
}

/// Paint the bottom of the grid that fits the window, with a block cursor:
/// only the rows inside `damage` (window pixels; `None` paints them all),
/// since only the damage reaches the window's frame. Returns the layout, so
/// the next drain can turn changed rows into a rectangle.
/// The metrics are design pixels, times the UI scale (`canvas.dpi() / 96`)
/// on screen, so the cells grow with the text (docs/hidpi-plan.md).
fn paint(canvas: &mut dyn Canvas, palette: &Palette, grid: &Grid, damage: Option<Rect>) -> Layout {
    let bounds = canvas.bounds();
    let damage = intersect(damage.unwrap_or(bounds), bounds);
    canvas.fill_rect(damage, palette.bg);
    let scale = (canvas.dpi() / 96).max(1) as i32;
    let (pad, line_h, cell_w) = (PAD * scale, LINE_H * scale, CELL_W * scale as f32);
    let visible_cols = ((bounds.width() - 2 * pad) as f32 / cell_w) as usize;
    let visible = (((bounds.height() - 2 * pad) / line_h).max(1) as usize).min(ROWS);
    let first = first_row(grid.row, visible);
    let last = (first + visible).min(ROWS);
    let row_rect = |slot: usize| {
        let top = bounds.top + pad + slot as i32 * line_h;
        Rect::new(bounds.left, top, bounds.right, top + line_h)
    };
    for (slot, line) in grid.cells[first..last].iter().enumerate() {
        if !overlaps(row_rect(slot), damage) {
            continue;
        }
        let text: String = line[..visible_cols.min(COLS)].iter().collect();
        let top = bounds.top + pad + slot as i32 * line_h;
        let rect = Rect::new(bounds.left + pad, top, bounds.right - pad, top + line_h);
        canvas.draw_text(&text, rect, &TextStyle::new(palette.fg, Dip(FONT)));
    }
    if grid.row >= first && grid.row < last && grid.col < visible_cols {
        let top = bounds.top + pad + (grid.row - first) as i32 * line_h;
        let left = bounds.left + pad + (grid.col as f32 * cell_w) as i32;
        let rect = Rect::new(
            left,
            top + line_h - 3 * scale,
            left + cell_w as i32,
            top + line_h,
        );
        if overlaps(rect, damage) {
            canvas.fill_rect(rect, palette.cursor);
        }
    }
    Layout {
        width: bounds.width(),
        height: bounds.height(),
        pad,
        line_h,
        visible,
    }
}
