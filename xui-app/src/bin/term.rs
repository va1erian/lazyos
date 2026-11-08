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
//! changes (settings in `confd` under `user/<uid>/term/`, or zoom), and
//! `TERM:RESIZE:<cols>x<rows>` each applied size. The conveniences report
//! `TERM:MENU:<item>`, `TERM:LINK:OPEN:<url>`, `TERM:FIND:<matches>`,
//! `TERM:BELL:<kind>`, `TERM:NEWWIN:<cwd>`, `TERM:CLOSE:CONFIRM|CANCEL|FORCE`,
//! `TERM:PREFS:SET:<key>` and `TERM:SAVE:<bytes>`.

use std::cell::{Cell, RefCell};
use std::os::unix::io::RawFd;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;
use std::rc::Rc;

use lazyterm::Rgb;
use lazyterm::Term as Engine;
use xui_app::backend::LazyOSBackend;
use xui_app::themed::run_themed;
use xui_core::app::Proxy;
use xui_core::backend::{Cursor, NodeKind, NodeSpec, PlatformSpec};
use xui_core::widget::{FileDialog, StdFileSystem};
use xui_core::{Control, Dip};

#[path = "term/actions.rs"]
mod actions;
#[path = "term/app.rs"]
mod app;
#[path = "term/appearance.rs"]
mod appearance;
#[path = "term/badge.rs"]
mod badge;
#[path = "term/bell.rs"]
mod bell;
#[path = "term/blink.rs"]
mod blink;
#[path = "term/chords.rs"]
mod chords;
#[path = "term/chrome.rs"]
mod chrome;
#[path = "term/clicks.rs"]
mod clicks;
#[path = "term/clip.rs"]
mod clip;
#[path = "term/conf.rs"]
mod conf;
#[path = "term/events.rs"]
mod events;
#[path = "term/evidence.rs"]
mod evidence;
#[path = "term/findbar.rs"]
mod findbar;
#[path = "term/findctl.rs"]
mod findctl;
#[path = "term/fonts.rs"]
mod fonts;
#[path = "term/geometry.rs"]
mod geometry;
#[path = "term/helpers.rs"]
mod helpers;
#[path = "term/jobs.rs"]
mod jobs;
#[path = "term/keys.rs"]
mod keys;
#[path = "term/linkui.rs"]
mod linkui;
#[path = "term/look.rs"]
mod look;
#[path = "term/menu.rs"]
mod menu;
#[path = "term/meta.rs"]
mod meta;
#[path = "term/metrics.rs"]
mod metrics;
#[path = "term/msg.rs"]
mod msg;
#[path = "term/output.rs"]
mod output;
#[path = "term/overlay.rs"]
mod overlay;
#[path = "term/paint.rs"]
mod paint;
#[path = "term/perf.rs"]
mod perf;
#[path = "term/pointer.rs"]
mod pointer;
#[path = "term/prefs.rs"]
mod prefs;
#[path = "term/prefs_paint.rs"]
mod prefs_paint;
#[path = "term/prompt.rs"]
mod prompt;
#[path = "term/pty.rs"]
mod pty;
#[path = "term/scroll.rs"]
mod scroll;
#[path = "term/timers.rs"]
mod timers;
#[path = "term/view.rs"]
mod view;

use app::Terminal;
use blink::Blink;
use chrome::Chrome;
use clicks::Clicks;
use clip::watch_drops;
use conf::Follower;
use evidence::Evidence;
use look::Look;
use msg::Msg;
use paint::{paint, Scene};
use perf::{PaintClock, Perf};
use pointer::probe_grid;
use timers::Timers;
use view::Shown;

/// The shell to host: the shipped `/system/bin/busybox`, started as
/// `busybox sh -i`. `argv[0]` (`fhs::boot::BUSYBOX_ARGV0`) is what BusyBox
/// dispatches on, so it is set explicitly.
const SHELL: &str = fhs::bin::BUSYBOX;
/// BusyBox ash's default prompt (`\w \$ `) behind a non-printing OSC 7 with
/// the working directory: `\[` and `\]` bracket what takes no columns, `\e`
/// and `\a` are ESC and BEL. The visible prompt is exactly the default one
/// (`evidence::is_prompt` reads it). `\w` abbreviates the home directory to
/// `~`, which `Terminal::cwd` expands.
const PROMPT: &str = "\\[\\e]7;file://\\w\\a\\]\\w \\$ ";
/// Window size when a compositor lays the app out; as the display owner the
/// terminal fills the screen instead.
const WINDOW: (i32, i32) = (640, 400);
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

fn main() {
    std::panic::set_hook(Box::new(|info| println!("TERM:PANIC:{info}")));
    // The shaper builds its font database on its first use, so every face is
    // registered now, before the backend exists: JetBrains Mono (built in) and
    // the monospace families under `/system/share/fonts` and the user's own.
    let fonts = Rc::new(fonts::register(&fonts::directories()));
    xui_canvas::set_default_family(fonts::BUILT_IN);
    let mut follower = Follower::new();
    let conf = follower.read();
    // A new window starts where its launcher said (`--cwd` or one directory
    // argument), else in the home directory.
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let start_dir = helpers::start_dir(std::env::args_os(), home.as_deref(), |p| p.is_dir());
    let backend = match LazyOSBackend::connect() {
        Ok(backend) => Rc::new(backend),
        Err(code) => {
            println!("TERM:BIND:FAIL:{code}");
            std::process::exit(1);
        }
    };
    // Closing the window while a program runs asks first, in the window: the
    // compositor keeps it until the Terminal quits.
    backend.ask_before_close();
    let (width, height) = backend.window_size(WINDOW);
    backend.on_first_frame(|| println!("TERM:UP:PASS"));

    let paint_clock = Rc::new(PaintClock::default());
    let layout = Rc::new(Cell::new(None));
    let offset = Rc::new(Cell::new(0usize));
    let timers = Timers::default();
    let chrome: Rc<RefCell<Chrome>> = Rc::default();
    // Files dropped on the window arrive on the backend's hook; the proxy
    // turns them into a message once the UI exists.
    let outbox: Rc<RefCell<Option<Proxy<Msg>>>> = Rc::default();
    watch_drops(&backend, Rc::clone(&outbox));
    // The window gaining or losing the keyboard (switching to another window):
    // the cursor goes hollow and a program that asked hears about it.
    {
        let outbox = Rc::clone(&outbox);
        backend.on_keyboard_focus(move |focused| {
            if let Some(proxy) = outbox.borrow().as_ref() {
                let _ = proxy.send(Msg::Focus(focused));
            }
        });
    }
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
        // The grid fills the window, unless a size was asked for.
        let asked = settings.initial_grid();
        let (cols, rows) = asked.unwrap_or_else(|| appearance::grid_for(ui.client_rect(), &metrics));
        let mut shell = Command::new(SHELL);
        shell.arg0(fhs::boot::BUSYBOX_ARGV0).args(["sh", "-i"]);
        shell.env("TERM", "xterm-256color");
        // 24-bit colour, the terminfo database the shipped entry lives in,
        // and a prompt that reports the working directory (OSC 7) without
        // changing what is shown (docs/terminal-plan.md T5).
        shell.env("COLORTERM", "truecolor");
        shell.env("TERMINFO_DIRS", fhs::share::TERMINFO);
        shell.env("PS1", PROMPT);
        if let Some(dir) = &start_dir {
            shell.current_dir(dir);
        }
        let (master, child) = match pty::spawn(shell, rows as u16, cols as u16) {
            Ok(spawned) => spawned,
            Err(error) => {
                println!("TERM:SPAWN:FAIL:{error}");
                backend.unbind();
                std::process::exit(1);
            }
        };
        set_nonblocking(master.fd());
        backend.watch_fd(master.fd());
        let term = Rc::new(RefCell::new(Engine::new(cols, rows)));
        {
            let term = Rc::clone(&term);
            let look = Rc::clone(&look);
            let theme = ui.theme_handle();
            let clock = Rc::clone(&paint_clock);
            let layout = Rc::clone(&layout);
            let offset = Rc::clone(&offset);
            let chrome = Rc::clone(&chrome);
            let backend = Rc::clone(&backend);
            root.set_painter(Rc::new(move |canvas| {
                let theme = theme.get();
                let accent = theme.accent;
                let (term, look, chrome) = (term.borrow(), look.borrow(), chrome.borrow());
                let scene = Scene {
                    term: &term,
                    look: &look,
                    offset: offset.get(),
                    accent: Rgb(accent.r, accent.g, accent.b),
                    chrome: &chrome,
                    dark: theme.is_dark,
                };
                let damage = backend.paint_damage();
                let laid = clock.time(|| paint(canvas, &scene, damage));
                // The probe line names the grid's rectangle; it moves when the
                // window is resized.
                if layout.replace(Some(laid)) != Some(laid) {
                    probe_grid(&laid);
                }
            }));
        }
        events::install(&root);
        // The grid is text: the I-beam over it.
        ui.set_cursor(root.id(), Cursor::Text);
        *outbox.borrow_mut() = Some(ui.proxy());
        root.focus();
        ui.on_close(|| Some(Msg::Close));
        events::watch_resize(ui);
        events::route_timers(ui, &timers);
        ui.set_timer(FALLBACK_MILLIS);
        // Save output: xui's Save dialog (an in-window modal, as the Editor's).
        let save_dialog = FileDialog::save_file(ui, "Save output")
            .expect("the save dialog's nodes")
            .file_system(Rc::new(StdFileSystem))
            .on_accept(|path| Some(Msg::SaveChosen(path)))
            .on_cancel(|| Some(Msg::SaveCancelled));
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
            out: Vec::new(),
            pending_text: None,
            look,
            conf: conf.clone(),
            fonts: Rc::clone(&fonts),
            follower,
            zoom: 0,
            dark: false,
            applied_scheme: None,
            marker: None,
            blink: Blink::default(),
            timers: timers.clone(),
            chrome: Rc::clone(&chrome),
            backend: Rc::clone(&backend),
            sized: false,
            settling: asked.is_some(),
            remember: false,
            save: None,
            skip_char: None,
            title_generation: 0,
            title: meta::DEFAULT_TITLE.to_string(),
            cwd: None,
            sync_hold: None,
            bell: bell::Limiter::default(),
            save_dialog,
            found_reported: None,
            hover: None,
        };
        terminal.restyle(ui);
        if let Some(grid) = asked {
            terminal.request_grid(grid);
        }
        terminal
    });

    backend.unbind();
    std::process::exit(i32::from(xui_app::launch::finish(xui_app::trace::Channel::Term, outcome)))
}
