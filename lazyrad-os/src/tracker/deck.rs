//! A deck: one song playing into one sink, as a form script sees it.
//!
//! The deck renders ahead of what is heard (the sink's buffer), so it keeps a
//! timeline of where the song was at each rendered frame and reports the
//! position, levels and timing of the frame being *played*: a pattern view
//! highlights the row you hear, not the row being mixed.

use std::collections::VecDeque;
use std::rc::Rc;

use modplay::{Module, Options, Player, CHANNELS};

use super::sink::Sink;

/// Frames rendered between two timeline entries: about 12 ms at 22.05 kHz,
/// finer than a tracker row at any speed a song can set.
const SLICE_FRAMES: usize = 256;

/// The most frames one pump renders, so a deck that fell far behind (or a
/// clock sink after a long stall) cannot stall the UI thread.
const MAX_PUMP_FRAMES: usize = 32 * 1024;

/// Where the song is at one frame, for the UI.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub order: usize,
    pub row: usize,
    pub pattern: usize,
    pub speed: u8,
    pub tempo: u8,
    pub levels: [u8; CHANNELS],
}

/// Where a deck is in its life.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Playing,
    Paused,
    /// The song played out; `on_end` is due once.
    Ended,
    /// `stop()`, a closed window or an audio failure: nothing more happens.
    Stopped,
}

/// What a deck plays: settings from the script's options map.
#[derive(Clone, Copy, Debug)]
pub struct Settings {
    pub options: Options,
    pub volume: u8,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            options: Options {
                loops: Some(1),
                separation: 50,
                interpolate: true,
            },
            volume: 100,
        }
    }
}

/// The playing half of a deck; the script half (callbacks) is in `events`.
pub struct Deck {
    module: Rc<Module>,
    player: Player<Rc<Module>>,
    sink: Box<dyn Sink>,
    state: State,
    volume: u8,
    /// `(first frame after the slice, where the song was during it)`.
    timeline: VecDeque<(u64, Snapshot)>,
    /// The snapshot of the frame being played.
    heard: Snapshot,
    /// The frame after the song's last one, once it has been rendered.
    end: Option<u64>,
    buf: Vec<i16>,
}

impl Deck {
    pub fn new(module: Rc<Module>, mut sink: Box<dyn Sink>, settings: Settings) -> Deck {
        let player = Player::new(Rc::clone(&module), sink.rate(), settings.options);
        let _ = sink.set_volume(settings.volume);
        let heard = snapshot(&player);
        Deck {
            module,
            player,
            sink,
            state: State::Playing,
            volume: settings.volume.min(100),
            timeline: VecDeque::new(),
            heard,
            end: None,
            buf: vec![0; SLICE_FRAMES * 2],
        }
    }

    pub fn state(&self) -> State {
        self.state
    }

    /// What the listener hears now.
    pub fn heard(&self) -> Snapshot {
        self.heard
    }

    /// The song, shared with the script's `Song` value.
    pub fn module(&self) -> &Rc<Module> {
        &self.module
    }

    /// Whether the frames go to a sound card (`false`: no mixer, silent).
    pub fn has_audio(&self) -> bool {
        self.sink.is_audio()
    }

    /// Milliseconds of the song heard so far.
    pub fn elapsed_ms(&self) -> u64 {
        self.sink.played() * 1000 / u64::from(self.sink.rate().max(1))
    }

    pub fn volume(&self) -> u8 {
        self.volume
    }

    /// Keep the sink full and move [`Deck::heard`] to the frame being played.
    /// An audio failure stops the deck and is returned.
    pub fn pump(&mut self) -> Result<(), String> {
        if self.state != State::Playing {
            return Ok(());
        }
        let result = self.fill();
        self.catch_up();
        if let Err(error) = result {
            self.stop();
            return Err(error);
        }
        if self.end.is_some_and(|end| self.sink.played() >= end) {
            self.state = State::Ended;
            self.heard.levels = [0; CHANNELS];
        }
        Ok(())
    }

    fn fill(&mut self) -> Result<(), String> {
        let mut budget = self.sink.free()?.min(MAX_PUMP_FRAMES);
        // Fill every free frame, the last slice short if need be: a mixer
        // stream starts only once its ring is full, so a ring left a few
        // frames short (after a pause re-queued its backlog, or a ring that
        // is not a whole number of slices) would never start.
        while budget > 0 {
            let want = budget.min(SLICE_FRAMES);
            let frames = if self.player.is_finished() {
                // Past the end: silence. The mixer reads a ring in whole
                // periods, so the song's last frames are only consumed once
                // something follows them.
                self.buf[..want * 2].fill(0);
                want
            } else {
                self.player.render(&mut self.buf[..want * 2])
            };
            let taken = self.sink.write(&self.buf[..frames * 2])?;
            let now = if self.player.is_finished() {
                // The player has wrapped to its restart order already; the
                // last slice still sounds where the song was.
                self.end
                    .get_or_insert(self.sink.written() - taken as u64 + frames as u64);
                Snapshot {
                    levels: [0; CHANNELS],
                    ..self.timeline.back().map_or(self.heard, |&(_, last)| last)
                }
            } else {
                snapshot(&self.player)
            };
            self.timeline.push_back((self.sink.written(), now));
            budget -= frames;
            if taken < frames || frames == 0 {
                break; // cannot happen: `free` said there was room
            }
        }
        if self.player.is_finished() {
            self.sink.flush()?;
        }
        Ok(())
    }

    /// Advance [`Deck::heard`] past every slice that has been played.
    fn catch_up(&mut self) {
        let played = self.sink.played();
        while let Some(&(end, snapshot)) = self.timeline.front() {
            if end > played {
                // The slice being played now.
                self.heard = snapshot;
                break;
            }
            self.heard = snapshot;
            self.timeline.pop_front();
        }
    }

    pub fn pause(&mut self) -> Result<(), String> {
        if self.state == State::Playing {
            self.state = State::Paused;
            self.sink.pause()?;
        }
        Ok(())
    }

    pub fn resume(&mut self) -> Result<(), String> {
        if self.state == State::Paused {
            self.state = State::Playing;
            self.sink.resume()?;
        }
        Ok(())
    }

    /// End for good: no more frames, no `on_end`.
    pub fn stop(&mut self) {
        if self.state != State::Stopped {
            let _ = self.sink.pause();
            self.state = State::Stopped;
            self.heard.levels = [0; CHANNELS];
        }
    }

    /// Jump to the start of `order`. Already-buffered frames still play (a
    /// fraction of a second), then the new position takes over.
    pub fn seek(&mut self, order: usize) {
        self.player.seek(order);
        self.end = None;
        if self.state == State::Ended {
            self.state = State::Playing;
        }
    }

    pub fn set_volume(&mut self, percent: u8) -> Result<(), String> {
        self.volume = percent.min(100);
        self.sink.set_volume(self.volume)
    }

    pub fn player(&self) -> &Player<Rc<Module>> {
        &self.player
    }

    pub fn player_mut(&mut self) -> &mut Player<Rc<Module>> {
        &mut self.player
    }
}

fn snapshot(player: &Player<Rc<Module>>) -> Snapshot {
    let position = player.position();
    Snapshot {
        order: position.order,
        row: position.row,
        pattern: player
            .module()
            .orders
            .get(position.order)
            .map_or(0, |&p| usize::from(p)),
        speed: player.speed(),
        tempo: player.tempo(),
        levels: player.levels(),
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;
    use crate::tracker::sink::{ClockSink, Sink};
    use crate::tracker::tests::demo_module;

    fn deck_with_clock(loops: Option<u32>) -> (Deck, Rc<Cell<u64>>) {
        let now = Rc::new(Cell::new(0));
        let reader = Rc::clone(&now);
        let sink = ClockSink::new(22_050, Box::new(move || reader.get()));
        let mut settings = Settings::default();
        settings.options.loops = loops;
        (Deck::new(demo_module(), Box::new(sink), settings), now)
    }

    #[test]
    fn the_heard_position_trails_the_rendered_one() {
        let (mut deck, now) = deck_with_clock(Some(1));
        deck.pump().unwrap();
        assert_eq!(deck.heard().row, 0, "nothing played yet");
        let rendered = deck.player().position();
        assert!(rendered.row > 0, "the buffer is ahead: {rendered:?}");
        // One row at speed 6, tempo 125 is 120 ms.
        now.set(250);
        deck.pump().unwrap();
        assert_eq!(deck.heard().row, 2);
        assert!(deck.elapsed_ms() >= 240 && deck.elapsed_ms() <= 250);
    }

    #[test]
    fn a_song_ends_once_its_last_frame_is_heard() {
        let (mut deck, now) = deck_with_clock(Some(1));
        let mut t = 0;
        while deck.state() == State::Playing {
            t += 40;
            now.set(t);
            deck.pump().unwrap();
            assert!(t < 10 * 60 * 1000, "the demo song ends");
        }
        assert_eq!(deck.state(), State::Ended);
        assert_eq!(deck.heard().levels, [0; CHANNELS]);
        assert_eq!(deck.heard().order, 2, "the last order, not the wrapped one");
        deck.seek(0);
        assert_eq!(deck.state(), State::Playing, "a seek plays it again");
    }

    #[test]
    fn a_pause_holds_the_position() {
        let (mut deck, now) = deck_with_clock(None);
        deck.pump().unwrap();
        now.set(500);
        deck.pump().unwrap();
        deck.pause().unwrap();
        let held = (deck.heard(), deck.elapsed_ms());
        now.set(5_000);
        deck.pump().unwrap();
        assert_eq!((deck.heard(), deck.elapsed_ms()), held);
        deck.resume().unwrap();
        now.set(5_250);
        deck.pump().unwrap();
        assert!(deck.elapsed_ms() > held.1);
    }

    #[test]
    fn seeking_moves_what_is_heard_after_the_buffer() {
        let (mut deck, now) = deck_with_clock(None);
        deck.pump().unwrap();
        deck.seek(2);
        let mut t = 0;
        while deck.heard().order != 2 {
            t += 40;
            now.set(t);
            deck.pump().unwrap();
            assert!(t < 2_000, "the seek is heard within the buffer");
        }
        assert_eq!(deck.heard().row, 0);
    }

    /// The state of a [`RingSink`], shared with the test that plays "mixer".
    #[derive(Default)]
    struct Ring {
        size: u64,
        written: u64,
        played: u64,
        started: bool,
    }

    impl Ring {
        /// The mixer reads up to `frames`, if the stream runs.
        fn consume(&mut self, frames: u64) {
            if self.started {
                self.played += frames.min(self.written - self.played);
            }
        }
    }

    /// Behaves like a mixer stream: plays only once its ring has been full
    /// (or it was flushed), and a pause keeps what was queued.
    struct RingSink(Rc<std::cell::RefCell<Ring>>);

    impl Sink for RingSink {
        fn rate(&self) -> u32 {
            22_050
        }
        fn is_audio(&self) -> bool {
            true
        }
        fn free(&mut self) -> Result<usize, String> {
            let ring = self.0.borrow();
            Ok((ring.size - (ring.written - ring.played)) as usize)
        }
        fn write(&mut self, samples: &[i16]) -> Result<usize, String> {
            let frames = (samples.len() / 2).min(self.free()?);
            let mut ring = self.0.borrow_mut();
            ring.written += frames as u64;
            ring.started |= ring.written - ring.played >= ring.size;
            Ok(frames)
        }
        fn played(&self) -> u64 {
            self.0.borrow().played
        }
        fn written(&self) -> u64 {
            self.0.borrow().written
        }
        fn flush(&mut self) -> Result<(), String> {
            self.0.borrow_mut().started = true;
            Ok(())
        }
        fn pause(&mut self) -> Result<(), String> {
            self.0.borrow_mut().started = false;
            Ok(())
        }
        fn resume(&mut self) -> Result<(), String> {
            Ok(())
        }
        fn set_volume(&mut self, _: u8) -> Result<(), String> {
            Ok(())
        }
    }

    #[test]
    fn a_ring_that_starts_only_when_full_is_filled_to_the_last_frame() {
        // 1000 frames: not a whole number of slices.
        let ring = Rc::new(std::cell::RefCell::new(Ring {
            size: 1000,
            ..Ring::default()
        }));
        let sink = RingSink(Rc::clone(&ring));
        let mut deck = Deck::new(demo_module(), Box::new(sink), Settings::default());
        deck.pump().unwrap();
        assert!(ring.borrow().started, "the first fill starts it");
        ring.borrow_mut().consume(300);
        deck.pump().unwrap();
        assert_eq!(
            ring.borrow().written - ring.borrow().played,
            1000,
            "topped up exactly"
        );
        // Paused with a few frames read since the last top-up: the backlog is
        // a little less than the ring.
        ring.borrow_mut().consume(3);
        deck.pause().unwrap();
        deck.resume().unwrap();
        deck.pump().unwrap();
        assert!(ring.borrow().started, "the top-up after a resume starts it");
        let before = deck.elapsed_ms();
        ring.borrow_mut().consume(500);
        deck.pump().unwrap();
        assert!(deck.elapsed_ms() > before, "it plays on after the pause");
    }

    #[test]
    fn a_mixer_reading_whole_periods_still_hears_the_end() {
        let ring = Rc::new(std::cell::RefCell::new(Ring {
            size: 4096,
            ..Ring::default()
        }));
        let mut deck = Deck::new(
            demo_module(),
            Box::new(RingSink(Rc::clone(&ring))),
            Settings::default(),
        );
        let mut periods = 0;
        while deck.state() == State::Playing {
            deck.pump().unwrap();
            // Only whole 1024-frame periods are read, like the mixer does.
            let queued = ring.borrow().written - ring.borrow().played;
            if queued >= 1024 {
                ring.borrow_mut().consume(1024);
            }
            periods += 1;
            assert!(periods < 100_000, "the end is heard");
        }
        assert_eq!(deck.state(), State::Ended);
        let song_frames = deck.elapsed_ms();
        assert!(song_frames > 20_000, "{song_frames} ms");
    }

    #[test]
    fn a_stopped_deck_does_nothing_more() {
        let (mut deck, now) = deck_with_clock(None);
        deck.pump().unwrap();
        deck.stop();
        let elapsed = deck.elapsed_ms();
        now.set(10_000);
        deck.pump().unwrap();
        deck.resume().unwrap();
        assert_eq!(deck.state(), State::Stopped);
        assert_eq!(deck.elapsed_ms(), elapsed);
    }
}
