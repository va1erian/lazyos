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
//! `TERM:SPAWN:FAIL:<reason>` when the child cannot start. `TERM:FONT:<family>:
//! <size>:<cols>x<rows>` reports the effective font and grid whenever either
//! changes (settings in `confd` under `user/<uid>/term/`, or zoom).

use std::cell::{Cell, RefCell};
use std::os::unix::io::RawFd;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command};
use std::rc::Rc;

use lazyterm::input::Button;
use lazyterm::Rgb;
use lazyterm::Term as Engine;
use xui_app::backend::LazyOSBackend;
use xui_app::themed::run_themed;
use xui_core::app::{App, Proxy, Ui};
use xui_core::backend::{Cursor, Event, NodeKind, NodeSpec, PlatformSpec, TimerId};
use xui_core::{Control, Dip, Key};

#[path = "term/appearance.rs"]
mod appearance;
#[path = "term/blink.rs"]
mod blink;
#[path = "term/clicks.rs"]
mod clicks;
#[path = "term/clip.rs"]
mod clip;
#[path = "term/conf.rs"]
mod conf;
#[path = "term/evidence.rs"]
mod evidence;
#[path = "term/fonts.rs"]
mod fonts;
#[path = "term/geometry.rs"]
mod geometry;
#[path = "term/keys.rs"]
mod keys;
#[path = "term/look.rs"]
mod look;
#[path = "term/metrics.rs"]
mod metrics;
#[path = "term/msg.rs"]
mod msg;
#[path = "term/output.rs"]
mod output;
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

use blink::Blink;
use clicks::Clicks;
use clip::watch_drops;
use conf::{Conf, Follower};
use evidence::{is_prompt, Evidence};
use look::Look;
use msg::Msg;
use paint::{paint, Scene};
use perf::{PaintClock, Perf};
use pointer::{mouse, probe_grid, MouseKind};
use view::{Layout, Shown};

/// The shell to host: the shipped `/system/bin/busybox`, started as
/// `busybox sh -i`. `argv[0]` (`fhs::boot::BUSYBOX_ARGV0`) is what BusyBox
/// dispatches on, so it is set explicitly.
const SHELL: &str = fhs::bin::BUSYBOX;
/// Window size when a compositor lays the app out; as the display owner the
/// terminal fills the screen instead.
const WINDOW: (i32, i32) = (640, 400);
/// The columns of the pty and the engine: BusyBox's line editor breaks a long
/// command at the margin with its own newline, and the evidence reads that, so
/// the width is not the window's until the resize phase (docs/terminal-plan.md
/// T4); the window clips what it cannot show. The rows are what the window
/// holds at the font's line height (`term/appearance.rs`).
const COLS: usize = 80;
/// A safety-net drain: the pty wakes the loop itself (`watch_fd`), so this
/// only matters if a wake were ever missed. The drain only repaints when
/// bytes actually arrived, so an idle terminal costs no frames.
const FALLBACK_MILLIS: u32 = 1000;
/// How often queued input is retried while the pty is full.
const FLUSH_MILLIS: u32 = 10;
/// The most bytes written to the pty at once (see `output.rs`).
const INPUT_CHUNK: usize = 1024;

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
    /// How the grid looks (shared with the painter), the settings in force,
    /// the families found, and what follows `confd`.
    look: Rc<RefCell<Look>>,
    conf: Conf,
    fonts: Rc<fonts::Fonts>,
    follower: Follower,
    /// This window's zoom steps (never stored), the theme's darkness and the
    /// scheme last given to the engine, and the last `TERM:FONT` line.
    zoom: i32,
    dark: bool,
    applied_scheme: Option<(String, bool)>,
    marker: Option<String>,
    /// The cursor blink and its timer, which runs only while it is needed.
    blink: Blink,
    blink_timer: Rc<Cell<Option<TimerId>>>,
}

impl Terminal {
    fn finish(&self, ui: &mut Ui<Msg>) {
        println!("TERM:EXIT:PASS");
        ui.quit();
    }

    /// Output arrived: feed the engine, answer its queries, report the
    /// evidence, repaint what changed.
    fn output(&mut self, ui: &mut Ui<Msg>, bytes: &[u8]) {
        let (top, completed, reply) = {
            let mut term = self.term.borrow_mut();
            let top = term.screen_top();
            (top, term.feed(bytes), term.take_reply())
        };
        self.follow(top);
        // Answers to cursor-position and device-attribute queries go back to
        // the program; ones that wait for them hang.
        if !reply.is_empty() {
            self.send(&reply);
        }
        for line in &completed {
            if let Some(marker) = self.evidence.line(line) {
                println!("TERM:OUT:{marker}");
            }
        }
        self.touch(ui);
        self.refresh(ui);
        let at_prompt = {
            let term = self.term.borrow();
            let row = term.row_text(term.cursor().0);
            !row.is_empty() && is_prompt(&row)
        };
        self.perf.read(bytes.len(), at_prompt, &self.paint_clock);
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
                    self.output(ui, &bytes);
                }
                if let Some(conf) = self.follower.poll() {
                    self.apply_conf(ui, conf);
                }
                self.follow_theme(ui);
                if self.hung_up {
                    self.finish(ui);
                }
            }
            Msg::Autoscroll => self.autoscroll(ui),
            Msg::Flush => self.flush_out(),
            Msg::Blink => self.blink_tick(ui),
            Msg::Focus(focused) => self.focus_changed(ui, focused),
            // While the paste confirmation is up the keyboard answers it.
            Msg::Char('\r' | '\n') if self.pending_text.is_some() => self.confirm(ui, true),
            Msg::Key(Key::ESCAPE, _) if self.pending_text.is_some() => self.confirm(ui, false),
            Msg::Char(_) | Msg::Key(..) if self.pending_text.is_some() => {}
            Msg::Char(ch) => {
                self.touch(ui);
                self.key(ch, ui);
            }
            // Both input paths also deliver Enter and Backspace as `Char`s:
            // handling the key too would send each one twice.
            Msg::Key(Key::RETURN | Key::BACK, _) => {}
            Msg::Key(key, mods) => {
                self.touch(ui);
                self.key_chord(ui, key, mods);
            }
            Msg::Wheel(delta, mods, x, y) => {
                self.touch(ui);
                self.wheel(ui, delta, mods, (x, y));
            }
            Msg::Mouse(mouse) => {
                self.touch(ui);
                self.mouse(ui, mouse);
            }
            Msg::Dropped(paths) => self.dropped(ui, paths),
            Msg::Close => self.finish(ui),
        }
        self.pump(ui);
    }
}

fn main() {
    std::panic::set_hook(Box::new(|info| println!("TERM:PANIC:{info}")));
    // The shaper builds its font database on its first use, so every face is
    // registered now, before the backend exists: JetBrains Mono (built in) and
    // the monospace families under `/system/share/fonts` and the user's own.
    let fonts = Rc::new(fonts::register(&fonts::directories()));
    xui_canvas::set_default_family(fonts::BUILT_IN);
    let mut follower = Follower::new();
    let conf = follower.read();
    let backend = match LazyOSBackend::connect() {
        Ok(backend) => Rc::new(backend),
        Err(code) => {
            println!("TERM:BIND:FAIL:{code}");
            std::process::exit(1);
        }
    };
    let (width, height) = backend.window_size(WINDOW);
    backend.on_first_frame(|| println!("TERM:UP:PASS"));

    let paint_clock = Rc::new(PaintClock::default());
    let layout = Rc::new(Cell::new(None));
    let offset = Rc::new(Cell::new(0usize));
    let auto: Rc<Cell<Option<TimerId>>> = Rc::default();
    let flush: Rc<Cell<Option<TimerId>>> = Rc::default();
    let blink_timer: Rc<Cell<Option<TimerId>>> = Rc::default();
    let pending: Rc<Cell<Option<usize>>> = Rc::default();
    // Files dropped on the window arrive on the backend's hook; the proxy
    // turns them into a message once the UI exists.
    let outbox: Rc<RefCell<Option<Proxy<Msg>>>> = Rc::default();
    watch_drops(&backend, Rc::clone(&outbox));
    let spec = PlatformSpec::new("Terminal").size(Dip(width as f32), Dip(height as f32));
    let outcome = run_themed(&backend, spec, |ui| {
        // `wants_tab`: Tab must reach the shell (completion), not move the
        // widget focus, which is all a Tab does for a node without it.
        let root = Control::new(
            ui,
            &NodeSpec::new(NodeKind::Custom, ui.client_rect()).wants_tab(),
        )
        .expect("root node");
        // The cell comes from the font, so the pty's rows (what the window
        // holds) are known only now.
        let settings = &conf.settings;
        let family = fonts.catalog.choose(&settings.family, fonts::BUILT_IN);
        let metrics = look::measure(ui, family, settings.zoomed_size(0) as f32, settings.line_spacing);
        let styles = fonts.catalog.styles(family).unwrap_or_default();
        let look = Rc::new(RefCell::new(Look::new(metrics, family, styles)));
        let height = ui.client_rect().height();
        let rows = (((height - 2 * metrics.pad) / metrics.line_h).max(1) as usize).min(200);
        let mut shell = Command::new(SHELL);
        shell.arg0(fhs::boot::BUSYBOX_ARGV0).args(["sh", "-i"]);
        shell.env("TERM", "vt100");
        let (master, child) = match pty::spawn(shell, rows as u16, COLS as u16) {
            Ok(spawned) => spawned,
            Err(error) => {
                println!("TERM:SPAWN:FAIL:{error}");
                backend.unbind();
                std::process::exit(1);
            }
        };
        set_nonblocking(master.fd());
        backend.watch_fd(master.fd());
        let term = Rc::new(RefCell::new(Engine::new(COLS, rows)));
        {
            let term = Rc::clone(&term);
            let look = Rc::clone(&look);
            let theme = ui.theme_handle();
            let clock = Rc::clone(&paint_clock);
            let layout = Rc::clone(&layout);
            let offset = Rc::clone(&offset);
            let pending = Rc::clone(&pending);
            let backend = Rc::clone(&backend);
            root.set_painter(Rc::new(move |canvas| {
                let accent = theme.get().accent;
                let (term, look) = (term.borrow(), look.borrow());
                let scene = Scene {
                    term: &term,
                    look: &look,
                    offset: offset.get(),
                    pending: pending.get(),
                    accent: Rgb(accent.r, accent.g, accent.b),
                };
                let damage = backend.paint_damage();
                let laid = clock.time(|| paint(canvas, &scene, damage));
                if layout.replace(Some(laid)).is_none() {
                    probe_grid(&laid);
                }
            }));
        }
        root.on_events(|event| match event {
            Event::Char(ch) => Some(Msg::Char(*ch)),
            Event::KeyDown { key, modifiers, .. } => Some(Msg::Key(*key, *modifiers)),
            Event::SetFocus => Some(Msg::Focus(true)),
            Event::KillFocus => Some(Msg::Focus(false)),
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
        let timers = (Rc::clone(&auto), Rc::clone(&flush), Rc::clone(&blink_timer));
        ui.on_timer(move |id| {
            Some(if timers.0.get() == Some(id) {
                Msg::Autoscroll
            } else if timers.1.get() == Some(id) {
                Msg::Flush
            } else if timers.2.get() == Some(id) {
                Msg::Blink
            } else {
                Msg::Tick
            })
        });
        ui.set_timer(FALLBACK_MILLIS);
        let mut terminal = Terminal {
            child,
            master,
            hung_up: false,
            term,
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
            look,
            conf: conf.clone(),
            fonts: Rc::clone(&fonts),
            follower,
            zoom: 0,
            dark: false,
            applied_scheme: None,
            marker: None,
            blink: Blink::default(),
            blink_timer: Rc::clone(&blink_timer),
        };
        terminal.restyle(ui);
        terminal
    });

    backend.unbind();
    std::process::exit(i32::from(xui_app::launch::finish("TERM", outcome)))
}
