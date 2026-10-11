//! The Video Player window: its messages, opening files, and the commands
//! that drive the player thread.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Instant;

use lazyvideo::{Player, Status};
use xui_core::app::{App, Ui};
use xui_core::backend::Result;
use xui_core::message::{Key, Modifiers};

use crate::host::Host;
use crate::ui::{self, Dialogs, Widgets, SEEK_MAX};

/// The window size the binary asks for.
pub const WINDOW: (i32, i32) = (800, 560);

/// How far the arrow keys seek.
const STEP_US: i64 = 5_000_000;
/// Ticks (16 ms each) the frame timer outlives playback, so the picture of
/// a seek made while paused still arrives.
const GRACE_TICKS: u32 = 30;

#[derive(Clone, Debug)]
pub enum Msg {
    Open,
    OpenChosen(PathBuf),
    DialogClosed,
    TogglePlay,
    Stop,
    /// The seek bar moved (thousandths of the duration).
    Scrub(f64),
    /// The seek bar was released or stepped.
    Seek(f64),
    /// Seek by this many microseconds.
    SeekBy(i64),
    /// The frame timer fired.
    Tick,
    /// Files were dropped on the window.
    Dropped(Vec<PathBuf>),
    Close,
}

pub struct VideoApp {
    host: Host,
    w: Widgets,
    dialogs: Dialogs,
    path: Option<PathBuf>,
    player: Option<Player>,
    /// The seek bar is being dragged: the app does not move it.
    scrubbing: bool,
    grace: u32,
    /// Where the last seek asked to go, until the player's position gets
    /// there: a relative seek in between starts from it, not from the stale
    /// position.
    pending_seek: Option<u64>,
    /// The last whole second logged, and the frame counter at that time.
    logged: Option<(u64, u64)>,
    clock: Instant,
    ended_logged: bool,
    dialog_open: Rc<Cell<bool>>,
    mirrored: RefCell<String>,
}

impl VideoApp {
    pub fn build(ui: &mut Ui<Msg>, host: Host) -> Result<VideoApp> {
        let (w, dialogs) = ui::build_window(ui, &host)?;
        ui.on_close(|| Some(Msg::Close));
        let dialog_open = Rc::new(Cell::new(false));
        {
            let dialog_open = Rc::clone(&dialog_open);
            ui.on_key(move |key, modifiers| {
                if dialog_open.get() {
                    None
                } else {
                    shortcut(key, modifiers)
                }
            });
        }
        w.view.set_note(
            "Open a video with Ctrl+O, or drop one here.\nMotion JPEG in AVI files play.",
            false,
        );
        w.seek.set_range(0.0, SEEK_MAX);
        w.seek.set_enabled(false);
        let app = VideoApp {
            host,
            w,
            dialogs,
            path: None,
            player: None,
            scrubbing: false,
            grace: 0,
            pending_seek: None,
            logged: None,
            clock: Instant::now(),
            ended_logged: false,
            dialog_open,
            mirrored: RefCell::new(String::new()),
        };
        app.w.view.focus(ui);
        Ok(app)
    }

    /// The player thread's status, if a video is open (tests read it).
    pub fn status(&self) -> Option<Status> {
        self.player.as_ref().map(Player::status)
    }

    /// Whether the frame timer is running (tests pump it by hand).
    pub fn ticking(&self) -> bool {
        self.w.view.timer_running()
    }

    /// Whether a picture is on screen, and its size.
    pub fn picture(&self) -> Option<(u32, u32)> {
        self.w.view.picture_size()
    }

    fn log(&self, line: &str) {
        (self.host.log)(line);
    }

    fn open_path(&mut self, path: PathBuf) {
        // The old player stops first: it holds the file and the decoder.
        self.player = None;
        match lazyvideo::open(&path) {
            Ok(media) => {
                let i = media.info.clone();
                self.log(&format!(
                    "VIDEO:OPEN:PASS:{}:{}x{}:{:.2}:{}:{}",
                    path.display(),
                    i.width,
                    i.height,
                    i.fps(),
                    i.frames,
                    i.codec_name()
                ));
                self.w.view.set_note("", false);
                self.player = Some(Player::start(media));
                self.w.seek.set_enabled(true);
                self.path = Some(path);
                self.ended_logged = false;
                self.logged = None;
                self.grace = GRACE_TICKS;
                self.w.view.run_timer(true);
            }
            Err(error) => {
                self.log(&format!("VIDEO:OPEN:FAIL:{}:{error}", path.display()));
                let text = format!("{} could not be opened: {error}.", file_name(&path));
                self.w.view.set_note(&text, true);
                self.w.seek.set_enabled(false);
                self.w.seek.set_value(0.0);
                self.path = None;
                self.w.view.run_timer(false);
            }
        }
        self.mirror(true);
    }

    fn toggle(&mut self) {
        let Some(player) = &self.player else { return };
        let status = player.status();
        if status.playing {
            player.pause();
            self.log("VIDEO:PAUSE");
        } else {
            player.play();
            self.ended_logged = false;
            self.log("VIDEO:PLAY");
        }
        self.wake();
    }

    fn stop(&mut self) {
        if let Some(player) = &self.player {
            player.pause();
            player.seek(0);
            self.pending_seek = Some(0);
            self.log("VIDEO:STOP");
            self.wake();
        }
    }

    fn seek_us(&mut self, us: u64) {
        if let Some(player) = &self.player {
            player.seek(us);
            self.pending_seek = Some(us);
            self.ended_logged = false;
            self.log(&format!("VIDEO:SEEK:{}", us / 1000));
            self.wake();
        }
    }

    /// Keeps the frame timer running through the next few ticks.
    fn wake(&mut self) {
        self.grace = GRACE_TICKS;
        self.w.view.run_timer(true);
    }

    /// Per frame-timer tick: take the newest picture, mirror the status and
    /// stop ticking once nothing moves.
    fn tick(&mut self) {
        let Some(player) = &self.player else {
            self.w.view.run_timer(false);
            return;
        };
        let got = self.w.view.pump(player);
        let status = player.status();
        let duration = player.info().duration_us();
        let near = 2 * player.info().frame_us();
        if self
            .pending_seek
            .is_some_and(|at| status.position_us.abs_diff(at) <= near)
        {
            self.pending_seek = None;
        }
        let (fps_pairs, shown) = (self.logged, status.shown);
        if status.playing {
            let second = status.position_us / 1_000_000;
            if fps_pairs.is_none_or(|(s, _)| s != second) {
                let seconds = self.clock.elapsed().as_secs_f64().max(0.001);
                let fps = fps_pairs.map_or(0.0, |(_, n)| (shown - n.min(shown)) as f64 / seconds);
                self.clock = Instant::now();
                self.log(&format!(
                    "VIDEO:POS:{}:shown={}:dropped={}:fps={:.0}",
                    status.position_us / 1000,
                    status.shown,
                    status.dropped,
                    fps
                ));
                self.logged = Some((second, shown));
            }
        }
        if status.ended && !self.ended_logged {
            self.ended_logged = true;
            self.log(&format!(
                "VIDEO:END:PASS:shown={}:dropped={}",
                status.shown, status.dropped
            ));
        }
        if let Some(error) = &status.error {
            self.log(&format!("VIDEO:ERROR:{error}"));
            let text = format!("Playback stopped: {error}.");
            self.w.view.set_note(&text, true);
            self.w.view.run_timer(false);
        } else if !status.playing && !got {
            self.grace = self.grace.saturating_sub(1);
            if self.grace == 0 {
                self.w.view.run_timer(false);
            }
        } else {
            self.grace = GRACE_TICKS;
        }
        if !self.scrubbing && duration > 0 {
            let at = status.position_us as f64 * SEEK_MAX / duration as f64;
            self.w.seek.set_value(at.min(SEEK_MAX));
        }
        self.mirror(false);
    }

    /// Mirrors the player onto the status bar.
    fn mirror(&self, force: bool) {
        let name = self
            .path
            .as_deref()
            .map_or("No video".to_owned(), file_name);
        let (time, format) = match &self.player {
            Some(player) => {
                let info = player.info();
                let status = player.status();
                let shown = if self.scrubbing {
                    self.seek_target_us(player)
                } else {
                    status.position_us
                };
                (
                    format!("{} / {}", clock(shown), clock(info.duration_us())),
                    format!(
                        "{}x{}  {}  {:.0} fps{}",
                        info.width,
                        info.height,
                        info.codec_name(),
                        info.fps(),
                        if status.dropped > 0 {
                            format!("  {} dropped", status.dropped)
                        } else {
                            String::new()
                        }
                    ),
                )
            }
            None => (String::new(), String::new()),
        };
        let line = format!("{name}|{time}|{format}");
        if force || *self.mirrored.borrow() != line {
            self.w.status.set_text(0, &name);
            self.w.status.set_text(1, &time);
            self.w.status.set_text(2, &format);
            *self.mirrored.borrow_mut() = line;
        }
        self.dialog_open.set(self.dialogs.any_open());
    }

    fn seek_target_us(&self, player: &Player) -> u64 {
        let value = self.w.seek.value().clamp(0.0, SEEK_MAX);
        (player.info().duration_us() as f64 * value / SEEK_MAX) as u64
    }
}

/// `m:ss` (or `h:mm:ss`) for a time in microseconds.
pub(crate) fn clock(us: u64) -> String {
    let s = us / 1_000_000;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

/// The keyboard: claimed ahead of the focused widget while no dialog is open.
pub fn shortcut(key: Key, m: Modifiers) -> Option<Msg> {
    let plain = !m.ctrl && !m.alt;
    match key {
        Key::O if m.ctrl => Some(Msg::Open),
        Key::SPACE if plain => Some(Msg::TogglePlay),
        Key::S if plain => Some(Msg::Stop),
        Key::LEFT if plain => Some(Msg::SeekBy(-STEP_US)),
        Key::RIGHT if plain => Some(Msg::SeekBy(STEP_US)),
        Key::HOME if plain => Some(Msg::Seek(0.0)),
        _ => None,
    }
}

fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

impl App for VideoApp {
    type Msg = Msg;

    fn update(&mut self, msg: Msg, ui: &mut Ui<Msg>) {
        match msg {
            Msg::Open => {
                if let Some(dir) = self.path.as_deref().and_then(Path::parent) {
                    self.dialogs.open.set_initial_dir(dir.to_path_buf());
                }
                self.dialogs.open.open();
            }
            Msg::OpenChosen(path) => {
                self.open_path(path);
                self.w.view.focus(ui);
            }
            Msg::Dropped(paths) => {
                if let Some(path) = paths.into_iter().next() {
                    self.open_path(path);
                }
            }
            Msg::DialogClosed => self.w.view.focus(ui),
            Msg::TogglePlay => self.toggle(),
            Msg::Stop => self.stop(),
            Msg::Scrub(_) => self.scrubbing = true,
            Msg::Seek(value) => {
                self.scrubbing = false;
                if let Some(player) = &self.player {
                    let us = (player.info().duration_us() as f64 * value.clamp(0.0, SEEK_MAX)
                        / SEEK_MAX) as u64;
                    self.seek_us(us);
                }
            }
            Msg::SeekBy(delta) => {
                if let Some(player) = &self.player {
                    let base = self.pending_seek.unwrap_or(player.status().position_us);
                    let at = base as i64 + delta;
                    let end = player.info().duration_us() as i64;
                    self.seek_us(at.clamp(0, end) as u64);
                }
            }
            Msg::Tick => self.tick(),
            Msg::Close => {
                self.log("VIDEO:QUIT:PASS");
                self.player = None;
                ui.quit();
                return;
            }
        }
        self.mirror(false);
    }
}
