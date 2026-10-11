//! The player thread: it owns the file, decodes, paces frames against the
//! media clock and leaves the newest one for the window to take.
//!
//! The window never blocks on the file or the decoder. It polls
//! [`Player::take_frame`] from a timer and reads [`Player::status`]; a frame
//! it did not take in time is replaced by a newer one (and counted as
//! dropped), which is the right thing for video.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::clock::{schedule, Clock, Schedule};
use crate::{frame_at, pts_us, Error, Media, VideoInfo};

/// Consecutive undecodable frames after which playback stops with an error.
const MAX_BAD_FRAMES: u32 = 30;

/// One decoded picture.
pub struct Frame {
    pub width: u32,
    pub height: u32,
    /// `width * height * 4` bytes, RGBA, opaque.
    pub rgba: Vec<u8>,
    /// Its place on the media timeline.
    pub pts_us: u64,
}

/// What the worker reports, readable at any time.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Status {
    pub position_us: u64,
    pub playing: bool,
    /// The last frame has been shown for its duration.
    pub ended: bool,
    /// Frames decoded and handed over.
    pub shown: u64,
    /// Frames skipped because the clock was ahead of the decoder.
    pub dropped: u64,
    /// The reason playback stopped on its own, if it did.
    pub error: Option<String>,
}

enum Command {
    Play,
    Pause,
    Seek(u64),
    Stop,
}

#[derive(Default)]
struct Shared {
    slot: Mutex<Option<Frame>>,
    /// Pixel buffers handed back by the window, to decode into again.
    spare: Mutex<Vec<Vec<u8>>>,
    position_us: AtomicU64,
    playing: AtomicBool,
    ended: AtomicBool,
    shown: AtomicU64,
    dropped: AtomicU64,
    error: Mutex<Option<String>>,
}

pub struct Player {
    commands: Sender<Command>,
    shared: Arc<Shared>,
    info: VideoInfo,
    thread: Option<JoinHandle<()>>,
}

impl Player {
    /// Starts playing `media` from the beginning.
    pub fn start(media: Media) -> Player {
        let info = media.info.clone();
        let (commands, inbox) = channel();
        let shared = Arc::new(Shared::default());
        shared.playing.store(true, Ordering::Relaxed);
        let worker = Worker::new(media, inbox, Arc::clone(&shared));
        let thread = thread::Builder::new()
            .name("video-player".into())
            .spawn(move || worker.run())
            .ok();
        Player {
            commands,
            shared,
            info,
            thread,
        }
    }

    pub fn info(&self) -> &VideoInfo {
        &self.info
    }

    pub fn play(&self) {
        let _ = self.commands.send(Command::Play);
    }

    pub fn pause(&self) {
        let _ = self.commands.send(Command::Pause);
    }

    /// Moves to `us`; the picture there shows at once, playing or paused.
    pub fn seek(&self, us: u64) {
        let _ = self.commands.send(Command::Seek(us));
    }

    /// The newest picture not yet taken.
    pub fn take_frame(&self) -> Option<Frame> {
        self.shared.slot.lock().ok()?.take()
    }

    /// Returns a taken picture's pixels so the decoder can reuse them.
    pub fn recycle(&self, rgba: Vec<u8>) {
        if let Ok(mut spare) = self.shared.spare.lock() {
            if spare.len() < 3 {
                spare.push(rgba);
            }
        }
    }

    pub fn status(&self) -> Status {
        let s = &self.shared;
        Status {
            position_us: s.position_us.load(Ordering::Relaxed),
            playing: s.playing.load(Ordering::Relaxed),
            ended: s.ended.load(Ordering::Relaxed),
            shown: s.shown.load(Ordering::Relaxed),
            dropped: s.dropped.load(Ordering::Relaxed),
            error: s.error.lock().ok().and_then(|e| e.clone()),
        }
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Stop);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct Worker {
    media: Media,
    inbox: Receiver<Command>,
    shared: Arc<Shared>,
    epoch: Instant,
    clock: Clock,
    /// The next frame to decode.
    next: usize,
    packet: Vec<u8>,
    bad_frames: u32,
    /// A seek asked for a picture to show even if the clock is paused.
    show_one: bool,
}

impl Worker {
    fn new(media: Media, inbox: Receiver<Command>, shared: Arc<Shared>) -> Worker {
        let mut clock = Clock::new();
        let epoch = Instant::now();
        clock.play(0);
        Worker {
            media,
            inbox,
            shared,
            epoch,
            clock,
            next: 0,
            packet: Vec::new(),
            bad_frames: 0,
            show_one: false,
        }
    }

    fn now_us(&self) -> u64 {
        self.epoch.elapsed().as_micros() as u64
    }

    fn run(mut self) {
        loop {
            let busy = self.clock.running() || self.show_one;
            // Idle (paused, or at the end): park until a command arrives.
            let received = if busy {
                let wait = self.pace();
                self.inbox.recv_timeout(wait)
            } else {
                self.inbox
                    .recv()
                    .map_err(|_| RecvTimeoutError::Disconnected)
            };
            match received {
                Ok(command) => {
                    if !self.command(command) {
                        return;
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
            self.publish_position();
        }
    }

    fn finished(&self) -> bool {
        self.next >= self.media.demux.frames()
    }

    /// Applies one command; `false` stops the thread.
    fn command(&mut self, command: Command) -> bool {
        let now = self.now_us();
        match command {
            Command::Stop => return false,
            Command::Play => {
                if self.shared.ended.load(Ordering::Relaxed) {
                    self.seek_to(0, now);
                }
                self.clock.play(now);
            }
            Command::Pause => self.clock.pause(now),
            Command::Seek(us) => {
                self.seek_to(us.min(self.media.info.duration_us()), now);
                self.show_one = true;
            }
        }
        true
    }

    fn seek_to(&mut self, us: u64, now: u64) {
        let info = &self.media.info;
        self.next = frame_at(us, info.fps_num, info.fps_den, self.media.demux.frames());
        self.clock.seek(us, now);
        self.shared.ended.store(false, Ordering::Relaxed);
        self.bad_frames = 0;
    }

    /// Does the due work for the next frame and returns how long to sleep
    /// before looking again.
    fn pace(&mut self) -> Duration {
        let now = self.now_us();
        let position = self.clock.position(now);
        if self.finished() {
            return self.end_of_stream(position);
        }
        let info = self.media.info.clone();
        let pts = pts_us(self.next as u64, info.fps_num, info.fps_den);
        let decision = if self.show_one {
            Schedule::Show
        } else {
            schedule(pts, position, info.frame_us())
        };
        match decision {
            Schedule::Wait(us) => Duration::from_micros(us),
            Schedule::Drop => {
                // Catch up: all frames are key frames, so skip to the one
                // the clock is at rather than decoding each in turn.
                let target = if self.media.demux.all_key() {
                    frame_at(
                        position,
                        info.fps_num,
                        info.fps_den,
                        self.media.demux.frames(),
                    )
                } else {
                    self.next + 1
                };
                let skipped = target.saturating_sub(self.next).max(1);
                self.shared
                    .dropped
                    .fetch_add(skipped as u64, Ordering::Relaxed);
                self.next += skipped;
                Duration::ZERO
            }
            Schedule::Show => {
                self.show_frame(pts);
                self.show_one = false;
                Duration::ZERO
            }
        }
    }

    fn end_of_stream(&mut self, position: u64) -> Duration {
        let end = self.media.info.duration_us();
        if position >= end {
            self.clock.pause(self.now_us());
            self.clock.seek(end, self.now_us());
            self.shared.position_us.store(end, Ordering::Relaxed);
            self.shared.ended.store(true, Ordering::Relaxed);
            self.shared.playing.store(false, Ordering::Relaxed);
            self.show_one = false;
            Duration::ZERO
        } else {
            Duration::from_micros(end - position)
        }
    }

    fn show_frame(&mut self, pts: u64) {
        let index = self.next;
        self.next += 1;
        let mut pixels = self
            .shared
            .spare
            .lock()
            .ok()
            .and_then(|mut spare| spare.pop())
            .unwrap_or_default();
        let result = self
            .media
            .demux
            .read(index, &mut self.packet)
            .and_then(|()| self.media.decoder.decode(&self.packet, &mut pixels));
        match result {
            Ok((width, height)) => {
                self.bad_frames = 0;
                let frame = Frame {
                    width,
                    height,
                    rgba: pixels,
                    pts_us: pts,
                };
                if let Ok(mut slot) = self.shared.slot.lock() {
                    if slot.replace(frame).is_some() {
                        // The window never took the previous one.
                        self.shared.dropped.fetch_add(1, Ordering::Relaxed);
                    }
                }
                self.shared.shown.fetch_add(1, Ordering::Relaxed);
            }
            Err(error) => self.bad_frame(error),
        }
    }

    fn bad_frame(&mut self, error: Error) {
        self.bad_frames += 1;
        self.shared.dropped.fetch_add(1, Ordering::Relaxed);
        if self.bad_frames >= MAX_BAD_FRAMES {
            if let Ok(mut slot) = self.shared.error.lock() {
                *slot = Some(error.to_string());
            }
            self.clock.pause(self.now_us());
            self.shared.playing.store(false, Ordering::Relaxed);
        }
    }

    fn publish_position(&self) {
        if !self.shared.ended.load(Ordering::Relaxed) {
            let position = self.clock.position(self.now_us());
            let end = self.media.info.duration_us();
            self.shared
                .position_us
                .store(position.min(end), Ordering::Relaxed);
        }
        self.shared.playing.store(
            self.clock.running() && self.shared.error.lock().map_or(true, |e| e.is_none()),
            Ordering::Relaxed,
        );
    }
}
