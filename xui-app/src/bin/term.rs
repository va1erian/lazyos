//! `xui-term`: the desktop Terminal (issues #216, #254), a `xuid` client that
//! hosts BusyBox `sh` as a real child process.
//!
//! A desktop session has no console: `xuid` owns the display grant, so the
//! kernel mux (where `logind`'s prompt lives) stops painting. The Terminal is a
//! windowed front end instead. It opens a pseudo-terminal (`term/pty.rs`),
//! starts `busybox sh -i` on its slave as the session's controlling terminal,
//! writes keystrokes to the master and feeds what the master reads to the
//! terminal engine (`lazyterm`, xui-app/crates/term: parser, attributed cell
//! grid, scroll regions, wide characters, the alternate screen). The event loop
//! parks on the master beside the compositor endpoints (`watch_fd`), so the
//! UI thread drains it the moment the shell writes, and repaints only the
//! rows that changed (`term/view.rs`).
//!
//! The shell is a real, out-of-process BusyBox on a real tty: pipes,
//! redirection and job control work as at a Linux terminal, `^C` is the line
//! discipline's `SIGINT` for the foreground job, programs that read cooked
//! input (`cat`, `dash`) get echo and line editing from the kernel,
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
use xui_core::app::Proxy;
use xui_core::backend::{Cursor, Event, NodeKind, NodeSpec, PlatformSpec, TimerId};
use xui_core::{Control, Dip, Key, Modifiers};

#[path = "term/clicks.rs"]
mod clicks;
#[path = "term/clip.rs"]
mod clip;
#[path = "term/evidence.rs"]
mod evidence;
#[path = "term/geometry.rs"]
mod geometry;
#[path = "term/keys.rs"]
mod keys;
#[path = "term/paint.rs"]
mod paint;
#[path = "term/perf.rs"]
mod perf;
#[path = "term/pointer.rs"]
mod pointer;
#[path = "term/pty.rs"]
mod pty;
#[path = "term/scroll.rs"]
mod scroll;
#[path = "term/view.rs"]
mod view;
use clicks::Clicks;
use clip::watch_drops;
use evidence::{is_prompt, Evidence};
use lazyterm::input::Button;
use lazyterm::Term as Engine;
use paint::{paint, DARK, LIGHT, LINE_H, PAD};
use perf::{PaintClock, Perf};
use pointer::{mouse, probe_grid, MouseKind, MouseMsg};
use scroll::{anchored, apply, Move};
use view::{max_offset, Layout, Shown};

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;

/// The shell to host: the shipped `/system/bin/busybox`, started as
/// `busybox sh -i`. `argv[0]` (`fhs::boot::BUSYBOX_ARGV0`) is what BusyBox
/// dispatches on, so it is set explicitly.
const SHELL: &str = fhs::bin::BUSYBOX;
/// Window size when a compositor lays the app out; as the display owner the
/// terminal fills the screen instead.
const WINDOW: (i32, i32) = (640, 400);
/// The terminal geometry BusyBox sees through `TIOCGWINSZ`: 80 columns and
/// at most 24 rows (the rows the window shows). The engine is created with
/// exactly the pty's size, so a program that addresses the screen from its
/// own idea of the height (`less` does not use the alternate screen) lands
/// where the shell expects; a taller engine left stale rows under the
/// prompt. Making the window resizable and the pty follow it is a later
/// phase (docs/terminal-plan.md T4).
const COLS: usize = 80;
const ROWS: usize = 24;
/// A safety-net drain: the pty wakes the loop itself (`watch_fd`), so this
/// only matters if a wake were ever missed. The drain only repaints when
/// bytes actually arrived, so an idle terminal costs no frames.
const FALLBACK_MILLIS: u32 = 1000;
/// How often queued input is retried while the pty is full.
const FLUSH_MILLIS: u32 = 10;
/// The most bytes written to the pty at once (see `flush_out`).
const INPUT_CHUNK: usize = 1024;

/// One application message.
enum Msg {
    /// The poll timer fired: drain output, maybe finish.
    Tick,
    /// A drag past the edge of the grid scrolls on this timer.
    Autoscroll,
    /// Input the pty had no room for is waiting: try again.
    Flush,
    /// A translated character from the keyboard.
    Char(char),
    /// A key press with the modifiers held: arrows, Escape and the like, and
    /// Ctrl+letter chords (which arrive without a `Char`). Enter and
    /// Backspace also arrive as `Char`s, which is the path that handles them.
    Key(Key, Modifiers),
    /// A wheel roll (`delta` is 120 a notch, positive up) at node pixel
    /// (`x`, `y`) with the modifiers held.
    Wheel(i16, Modifiers, i32, i32),
    /// A pointer press, release, move or double-click.
    Mouse(MouseMsg),
    /// Files dropped on the window.
    Dropped(Vec<PathBuf>),
    Close,
}

/// Put a descriptor into non-blocking mode so the poll timer's read never
/// stalls the UI thread.
fn set_nonblocking(fd: RawFd) {
    // Safety: `fcntl(F_SETFL)` on a descriptor this process owns; it only
    // changes the open-file flags and cannot fail in a way that corrupts memory.
    unsafe { libc::fcntl(fd, libc::F_SETFL, libc::O_NONBLOCK) };
}

/// The app: the child shell, the pty master, and the engine it paints from.
struct Terminal {
    child: Child,
    master: pty::Master,
    /// The shell side is gone (the master read `EIO` or end-of-file).
    hung_up: bool,
    term: Rc<RefCell<Engine>>,
    typed: String,
    /// Exactly one `TERM:OUT` per command (`term/evidence.rs`).
    evidence: Evidence,
    root: Control<Msg>,
    /// What is on screen, so a drain repaints only the rows that changed;
    /// the painter's latest layout feeds it.
    shown: Shown,
    layout: Rc<Cell<Option<Layout>>>,
    /// `TERM:PERF` evidence (`term/perf.rs`).
    perf: Perf,
    paint_clock: Rc<PaintClock>,
    /// Lines the view is scrolled back from the bottom, shared with the
    /// painter.
    offset: Rc<Cell<usize>>,
    /// A selection drag is in progress, the pointer's last position, the
    /// button a program sees held, and the triple-click memory.
    drag: bool,
    last_mouse: (i32, i32),
    held: Option<Button>,
    clicks: Clicks,
    /// The autoscroll timer while a drag needs it (shared with `on_timer`).
    auto: Rc<Cell<Option<TimerId>>>,
    /// Bytes for the child the pty would not take yet (a big paste), and the
    /// timer that retries while there are any.
    out: Vec<u8>,
    flush: Rc<Cell<Option<TimerId>>>,
    /// The paste awaiting confirmation: its text, and its line count for the
    /// painter's bar.
    pending_text: Option<String>,
    pending: Rc<Cell<Option<usize>>>,
}

impl Terminal {
    /// Write to the pty without touching the view (answers to queries). What
    /// the pty has no room for stays queued (`flush_out`, `pump`).
    fn send(&mut self, bytes: &[u8]) {
        self.out.extend_from_slice(bytes);
        self.flush_out();
    }

    /// Write the next chunk of the queue. The kernel's line discipline drops
    /// input it has no room for (4096 bytes) instead of pushing back on the
    /// writer, so a big paste goes in `INPUT_CHUNK` bytes at a time, one per
    /// `FLUSH_MILLIS`, and the shell reads each before the next arrives.
    fn flush_out(&mut self) {
        while !self.out.is_empty() {
            let chunk = self.out.len().min(INPUT_CHUNK);
            match self.master.file().write(&self.out[..chunk]) {
                Ok(0) => break,
                Ok(n) => {
                    self.out.drain(..n);
                    break;
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                // The shell is gone: the drain notices; there is no one to write to.
                Err(_) => self.out.clear(),
            }
        }
    }

    /// Run the retry timer while input is queued, and not otherwise.
    fn pump(&mut self, ui: &mut Ui<Msg>) {
        match (self.out.is_empty(), self.flush.get()) {
            (false, None) => self.flush.set(Some(ui.set_timer(FLUSH_MILLIS))),
            (true, Some(id)) => {
                ui.kill_timer(id);
                self.flush.set(None);
            }
            _ => {}
        }
    }

    /// Write what the user typed: the view returns to the bottom first.
    fn input(&mut self, ui: &mut Ui<Msg>, bytes: &[u8]) {
        self.scroll(ui, Move::Bottom);
        self.send(bytes);
    }

    /// Repaint what changed on screen (the rows, or the scrollbar).
    fn refresh(&mut self, ui: &mut Ui<Msg>) {
        let changed = self
            .shown
            .update(&self.term.borrow(), self.layout.get(), self.offset.get());
        if let Some(rect) = changed {
            ui.invalidate_rect(self.root.id(), rect);
        }
    }

    /// Move the view on the user's request: `TERM:SCROLL:<offset>` when it
    /// changes. A full-screen program has no scrollback, so nothing moves.
    fn scroll(&mut self, ui: &mut Ui<Msg>, mv: Move) {
        let max = max_offset(&self.term.borrow(), self.page());
        let offset = apply(mv, self.offset.get(), max);
        if offset != self.offset.get() {
            self.offset.set(offset);
            println!("TERM:SCROLL:{offset}");
            self.refresh(ui);
        }
    }

    /// Keep the view on its text while output arrives: `top` was the
    /// screen's top line before it.
    fn follow(&mut self, top: u64) {
        let term = self.term.borrow();
        let max = max_offset(&term, self.page());
        self.offset
            .set(anchored(self.offset.get(), top, term.screen_top(), max));
    }

    /// The lines a page scroll moves: what the window shows.
    fn page(&self) -> usize {
        self.layout.get().map_or(ROWS, |layout| layout.visible)
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
                    let (top, completed, reply) = {
                        let mut term = self.term.borrow_mut();
                        let top = term.screen_top();
                        (top, term.feed(&bytes), term.take_reply())
                    };
                    self.follow(top);
                    // Answers to cursor-position and device-attribute queries
                    // go back to the program; ones that wait for them hang.
                    if !reply.is_empty() {
                        self.send(&reply);
                    }
                    for line in &completed {
                        if let Some(marker) = self.evidence.line(line) {
                            println!("TERM:OUT:{marker}");
                        }
                    }
                    self.refresh(ui);
                    let at_prompt = {
                        let term = self.term.borrow();
                        let row = term.row_text(term.cursor().0);
                        !row.is_empty() && is_prompt(&row)
                    };
                    self.perf.read(bytes.len(), at_prompt, &self.paint_clock);
                }
                if self.hung_up {
                    self.finish(ui);
                }
            }
            Msg::Autoscroll => self.autoscroll(ui),
            Msg::Flush => self.flush_out(),
            // While the paste confirmation is up the keyboard answers it.
            Msg::Char('\r' | '\n') if self.pending_text.is_some() => self.confirm(ui, true),
            Msg::Key(Key::ESCAPE, _) if self.pending_text.is_some() => self.confirm(ui, false),
            Msg::Char(_) | Msg::Key(..) if self.pending_text.is_some() => {}
            Msg::Char(ch) => self.key(ch, ui),
            // Both input paths also deliver Enter and Backspace as `Char`s:
            // handling the key too would send each one twice.
            Msg::Key(Key::RETURN | Key::BACK, _) => {}
            Msg::Key(key, mods) => self.key_chord(ui, key, mods),
            Msg::Wheel(delta, mods, x, y) => self.wheel(ui, delta, mods, (x, y)),
            Msg::Mouse(mouse) => self.mouse(ui, mouse),
            Msg::Dropped(paths) => self.dropped(ui, paths),
            Msg::Close => self.finish(ui),
        }
        self.pump(ui);
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
    let rows = (((height - 2 * PAD) / LINE_H).max(1) as usize).min(ROWS);
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

    let term = Rc::new(RefCell::new(Engine::new(cols, rows)));
    let paint_clock = Rc::new(PaintClock::default());
    let layout = Rc::new(Cell::new(None));
    let offset = Rc::new(Cell::new(0usize));
    let auto: Rc<Cell<Option<TimerId>>> = Rc::default();
    let flush: Rc<Cell<Option<TimerId>>> = Rc::default();
    let pending: Rc<Cell<Option<usize>>> = Rc::default();
    // Files dropped on the window arrive on the backend's hook; the proxy
    // turns them into a message once the UI exists.
    let outbox: Rc<RefCell<Option<Proxy<Msg>>>> = Rc::default();
    watch_drops(&backend, Rc::clone(&outbox));
    backend.watch_fd(master.fd());
    let spec = PlatformSpec::new("Terminal").size(Dip(width as f32), Dip(height as f32));
    let outcome = run_themed(&backend, spec, |ui| {
        // `wants_tab`: Tab must reach the shell (completion), not move the
        // widget focus, which is all a Tab does for a node without it.
        let root = Control::new(
            ui,
            &NodeSpec::new(NodeKind::Custom, ui.client_rect()).wants_tab(),
        )
        .expect("root node");
        {
            let term = Rc::clone(&term);
            let theme = ui.theme_handle();
            let clock = Rc::clone(&paint_clock);
            let layout = Rc::clone(&layout);
            let offset = Rc::clone(&offset);
            let pending = Rc::clone(&pending);
            let backend = Rc::clone(&backend);
            root.set_painter(Rc::new(move |canvas| {
                let palette = if theme.get().is_dark { &DARK } else { &LIGHT };
                let damage = backend.paint_damage();
                let laid = clock.time(|| {
                    paint(canvas, palette, &term.borrow(), offset.get(), pending.get(), damage)
                });
                if layout.replace(Some(laid)).is_none() {
                    probe_grid(&laid);
                }
            }));
        }
        root.on_events(|event| match event {
            Event::Char(ch) => Some(Msg::Char(*ch)),
            Event::KeyDown { key, modifiers, .. } => Some(Msg::Key(*key, *modifiers)),
            Event::MouseWheel {
                delta,
                horizontal: false,
                x,
                y,
                modifiers,
            } => Some(Msg::Wheel(*delta, *modifiers, *x, *y)),
            Event::MouseDown {
                x,
                y,
                button,
                modifiers,
            } => Some(mouse(MouseKind::Down(*button), *x, *y, *modifiers)),
            Event::MouseUp {
                x,
                y,
                button,
                modifiers,
            } => Some(mouse(MouseKind::Up(*button), *x, *y, *modifiers)),
            Event::MouseDoubleClick {
                x,
                y,
                button,
                modifiers,
            } => Some(mouse(MouseKind::Double(*button), *x, *y, *modifiers)),
            Event::MouseMove { x, y, modifiers } => {
                Some(mouse(MouseKind::Move, *x, *y, *modifiers))
            }
            _ => None,
        });
        // The grid is text: the I-beam over it.
        ui.set_cursor(root.id(), Cursor::Text);
        *outbox.borrow_mut() = Some(ui.proxy());
        root.focus();
        ui.on_close(|| Some(Msg::Close));
        let (timer_auto, timer_flush) = (Rc::clone(&auto), Rc::clone(&flush));
        ui.on_timer(move |id| {
            Some(if timer_auto.get() == Some(id) {
                Msg::Autoscroll
            } else if timer_flush.get() == Some(id) {
                Msg::Flush
            } else {
                Msg::Tick
            })
        });
        ui.set_timer(FALLBACK_MILLIS);
        Terminal {
            child,
            master,
            hung_up: false,
            term: Rc::clone(&term),
            typed: String::new(),
            evidence: Evidence::default(),
            root,
            shown: Shown::default(),
            layout: Rc::clone(&layout),
            perf: Perf::default(),
            paint_clock: Rc::clone(&paint_clock),
            offset: Rc::clone(&offset),
            drag: false,
            last_mouse: (0, 0),
            held: None,
            clicks: Clicks::default(),
            auto: Rc::clone(&auto),
            out: Vec::new(),
            flush: Rc::clone(&flush),
            pending_text: None,
            pending: Rc::clone(&pending),
        }
    });

    backend.unbind();
    std::process::exit(i32::from(xui_app::launch::finish("TERM", outcome)))
}
