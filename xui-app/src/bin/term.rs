//! `xui-term`: the desktop Terminal (issues #216, #254), a `xuid` client that
//! hosts BusyBox `sh` as a real child process.
//!
//! A desktop session has no console: `xuid` owns the display grant, so the
//! kernel mux (where `logind`'s prompt lives) stops painting. The Terminal is a
//! windowed front end instead. It spawns `/busybox sh` with a pipe pair for
//! stdin/stdout/stderr (the kernel reports fds 0/1/2 as a terminal, so BusyBox
//! runs its interactive line editor), sends keystrokes to the child's stdin,
//! and parses the child's output into a character grid. A poll timer drains the
//! non-blocking reads on the UI thread — the kernel does not share a descriptor
//! table between threads, so a reader thread would lose the pipe fds.
//!
//! This replaces the in-process interpreter the Terminal used to link: the
//! shell is a real, out-of-process BusyBox, so pipes and redirection
//! work exactly as at the console. There is no controlling tty yet, so job
//! control (a tty's `SIGINT` on `^C`) is limited to forwarding the control byte
//! and signalling the child; line editing and echo come from BusyBox itself.
//!
//! Serial evidence: `TERM:UP:PASS` after the first frame, `TERM:CMD:<line>` for
//! each submitted command, `TERM:OUT:<line>` for each completed output line,
//! `TERM:EXIT:PASS` when the shell exits or the window is closed, and
//! `TERM:SPAWN:FAIL:<reason>` when the child cannot start.

use std::io::{Read, Write};
use std::os::unix::io::{AsRawFd, RawFd};
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, Stdio};

use xui_app::backend::LazyOSBackend;
use xui_core::app::{run_app, App, Ui};
use xui_core::backend::{Backend, Event, NodeKind, NodeSpec, PlatformSpec};
use xui_core::{Canvas, Color, Control, Dip, Key, Rect, TextStyle};

#[path = "term/grid.rs"]
mod grid;
use grid::{is_prompt, Grid, COLS, ROWS};

use std::cell::RefCell;
use std::rc::Rc;

/// The shell to host: a bare applet name the kernel's Linux loader aliases to
/// the shipped `BUSYBOX`.
const SHELL: &str = "/busybox";
/// Window size when a compositor lays the app out; as the display owner the
/// terminal fills the screen instead.
const WINDOW: (i32, i32) = (640, 400);
/// Text size and the monospace cell it implies (JetBrains Mono is 0.6 em wide).
const FONT: f32 = 15.0;
const CELL_W: f32 = FONT * 0.6;
const LINE_H: i32 = 20;
const PAD: i32 = 10;
/// How often the app drains the child's output. The drain only repaints when
/// bytes actually arrived, so an idle terminal costs no frames.
const POLL_MILLIS: u32 = 100;

const BG: Color = Color::rgb(0x16, 0x18, 0x1d);
const FG: Color = Color::rgb(0xd7, 0xdb, 0xe0);
const CURSOR: Color = Color::rgb(0x6c, 0xb6, 0xff);

/// One application message.
enum Msg {
    /// The poll timer fired: drain output, maybe finish.
    Tick,
    /// A translated character from the keyboard.
    Char(char),
    /// A non-text key (Enter/Backspace via the key path).
    Key(Key),
    Close,
}

/// Put a descriptor into non-blocking mode so the poll timer's read never
/// stalls the UI thread.
fn set_nonblocking(fd: RawFd) {
    // Safety: `fcntl(F_SETFL)` on a descriptor this process owns; it only
    // changes the open-file flags and cannot fail in a way that corrupts memory.
    unsafe { libc::fcntl(fd, libc::F_SETFL, libc::O_NONBLOCK) };
}

/// The app: the child shell, its pipes, and the grid it paints on.
struct Terminal {
    child: Child,
    stdin: ChildStdin,
    stdout: ChildStdout,
    stderr: ChildStderr,
    out_eof: bool,
    err_eof: bool,
    grid: Rc<RefCell<Grid>>,
    typed: String,
    /// The last submitted command and whether its first output line is still
    /// expected, so exactly one `TERM:OUT` is printed per command (the shell
    /// echo and the bare prompt are skipped).
    last_cmd: Option<String>,
    awaiting_output: bool,
    root: Control<Msg>,
}

impl Terminal {
    fn send(&mut self, bytes: &[u8]) {
        let _ = self.stdin.write_all(bytes);
    }

    /// Drain both child pipes without blocking; return the bytes read.
    fn drain(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        let mut chunk = [0u8; 4096];
        if !self.out_eof {
            match self.stdout.read(&mut chunk) {
                Ok(0) => self.out_eof = true,
                Ok(n) => out.extend_from_slice(&chunk[..n]),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(_) => self.out_eof = true,
            }
        }
        if !self.err_eof {
            match self.stderr.read(&mut chunk) {
                Ok(0) => self.err_eof = true,
                Ok(n) => out.extend_from_slice(&chunk[..n]),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(_) => self.err_eof = true,
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
                    println!("TERM:CMD:{line}");
                    self.last_cmd = Some(line);
                    self.awaiting_output = true;
                }
            }
            '\u{8}' => {
                self.typed.pop();
                self.send(b"\x7f");
            }
            '\u{3}' => {
                self.send(b"\x03"); // ^C
                                    // Safety: `kill` with a pid we own; a non-negative pid names the child.
                unsafe { libc::kill(self.child.id() as i32, libc::SIGINT) };
            }
            '\u{4}' => self.send(b"\x04"), // ^D
            '\t' => self.send(b"\t"),
            c if c.is_control() => {}
            c => {
                self.typed.push(c);
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
                    ui.invalidate(self.root.id());
                }
                if self.out_eof && self.err_eof {
                    self.finish(ui);
                }
            }
            Msg::Char(ch) => self.key(ch),
            Msg::Key(Key::RETURN) => self.key('\n'),
            Msg::Key(Key::BACK) => self.key('\u{8}'),
            Msg::Key(Key::ESCAPE) => self.send(b"\x1b"),
            Msg::Key(_) => {}
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

    let mut child = match Command::new(SHELL)
        .args(["sh", "-i"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            println!("TERM:SPAWN:FAIL:{error}");
            backend.unbind();
            std::process::exit(1);
        }
    };
    let stdin = child.stdin.take().expect("piped stdin");
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    set_nonblocking(stdout.as_raw_fd());
    set_nonblocking(stderr.as_raw_fd());

    let grid = Rc::new(RefCell::new(Grid::new()));
    let spec = PlatformSpec::new("Terminal").size(Dip(width as f32), Dip(height as f32));
    let outcome = run_app(Rc::clone(&backend) as Rc<dyn Backend>, spec, |ui| {
        let root = Control::new(ui, &NodeSpec::new(NodeKind::Custom, ui.client_rect()))
            .expect("root node");
        {
            let grid = Rc::clone(&grid);
            root.set_painter(Rc::new(move |canvas| paint(canvas, &grid.borrow())));
        }
        root.on_events(|event| match event {
            Event::Char(ch) => Some(Msg::Char(*ch)),
            Event::KeyDown { key, .. } => Some(Msg::Key(*key)),
            _ => None,
        });
        root.focus();
        ui.on_close(|| Some(Msg::Close));
        ui.on_timer(|_| Some(Msg::Tick));
        ui.set_timer(POLL_MILLIS);
        Terminal {
            child,
            stdin,
            stdout,
            stderr,
            out_eof: false,
            err_eof: false,
            grid: Rc::clone(&grid),
            typed: String::new(),
            last_cmd: None,
            awaiting_output: false,
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

/// Paint the bottom of the grid that fits the window, with a block cursor.
fn paint(canvas: &mut dyn Canvas, grid: &Grid) {
    let bounds = canvas.bounds();
    canvas.clear(BG);
    let visible_cols = ((bounds.width() - 2 * PAD) as f32 / CELL_W) as usize;
    let visible = (((bounds.height() - 2 * PAD) / LINE_H).max(1) as usize).min(ROWS);
    // Show the top of the grid while it is not full, then scroll with the
    // cursor so the newest line stays visible.
    let first = if grid.row < visible {
        0
    } else {
        (grid.row + 1 - visible).min(ROWS - visible)
    };
    let last = (first + visible).min(ROWS);
    for (slot, line) in grid.cells[first..last].iter().enumerate() {
        let text: String = line[..visible_cols.min(COLS)].iter().collect();
        let top = bounds.top + PAD + slot as i32 * LINE_H;
        let rect = Rect::new(bounds.left + PAD, top, bounds.right - PAD, top + LINE_H);
        canvas.draw_text(&text, rect, &TextStyle::new(FG, Dip(FONT)));
    }
    if grid.row >= first && grid.row < last && grid.col < visible_cols {
        let top = bounds.top + PAD + (grid.row - first) as i32 * LINE_H;
        let left = bounds.left + PAD + (grid.col as f32 * CELL_W) as i32;
        let rect = Rect::new(left, top + LINE_H - 3, left + CELL_W as i32, top + LINE_H);
        canvas.fill_rect(rect, CURSOR);
    }
}
