//! Row/tick sequencing and the render loop.

use crate::channel::{Channel, Flow};
use crate::mixer::{mix, Pan};
use crate::module::{Module, CHANNELS, ROWS_PER_PATTERN};

/// Player settings.
#[derive(Clone, Copy, Debug)]
pub struct Options {
    /// Full plays of the song before it ends; `None` plays forever.
    pub loops: Option<u32>,
    /// Stereo separation, `0` (mono) to `100` (hard Amiga panning).
    pub separation: u8,
    /// Linear interpolation between sample bytes.
    pub interpolate: bool,
}

impl Default for Options {
    fn default() -> Options {
        Options {
            loops: Some(1),
            separation: 70,
            interpolate: true,
        }
    }
}

/// Where the song is, for a UI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Position {
    pub order: usize,
    pub row: usize,
}

const DEFAULT_SPEED: u8 = 6;
const DEFAULT_TEMPO: u8 = 125;
/// Orders (up to 128) times rows, one bit each.
const VISITED_WORDS: usize = 128 * ROWS_PER_PATTERN / 64;

/// Renders one module. Create it with [`Player::new`], then call
/// [`Player::render`] until it returns `0`.
pub struct Player<'a> {
    module: &'a Module,
    rate: u32,
    options: Options,
    pan: Pan,
    channels: [Channel; CHANNELS],
    speed: u8,
    tempo: u8,
    order: usize,
    row: usize,
    tick: u16,
    /// Extra repeats of the current row (`EEx`).
    delay: u8,
    flow: Flow,
    /// Carries the remainder of `rate * 5 / (2 * tempo)` so ticks do not drift.
    tick_remainder: u32,
    left_in_tick: u32,
    visited: [u64; VISITED_WORDS],
    plays: u32,
    finished: bool,
    unsupported: u32,
}

impl<'a> Player<'a> {
    /// A player at the start of `module`, mixing at `rate` Hz (clamped to
    /// 8000..=192000, the range the audio driver accepts).
    pub fn new(module: &'a Module, rate: u32, options: Options) -> Player<'a> {
        let mut player = Player {
            module,
            rate: rate.clamp(8_000, 192_000),
            options,
            pan: Pan::new(options.separation),
            channels: [Channel::default(); CHANNELS],
            speed: DEFAULT_SPEED,
            tempo: DEFAULT_TEMPO,
            order: 0,
            row: 0,
            tick: 0,
            delay: 0,
            flow: Flow::default(),
            tick_remainder: 0,
            left_in_tick: 0,
            visited: [0; VISITED_WORDS],
            plays: 0,
            finished: false,
            unsupported: 0,
        };
        player.mark_visited();
        player
    }

    /// The row being played.
    pub fn position(&self) -> Position {
        Position {
            order: self.order,
            row: self.row,
        }
    }

    /// Whether the song has ended (all requested plays done).
    pub fn is_finished(&self) -> bool {
        self.finished
    }

    /// Cells seen so far that used an effect this player ignores.
    pub fn unsupported_effects(&self) -> u32 {
        self.unsupported
    }

    /// Current volume of each channel, `0..=64` (for level meters).
    pub fn levels(&self) -> [u8; CHANNELS] {
        core::array::from_fn(|i| self.channels[i].volume())
    }

    /// Fill `out` with interleaved stereo frames. Returns the frames written,
    /// fewer than `out.len() / 2` only when the song has ended.
    pub fn render(&mut self, out: &mut [i16]) -> usize {
        let frames = out.len() / 2;
        let mut done = 0;
        while done < frames && !self.finished {
            if self.left_in_tick == 0 {
                self.begin_tick();
            }
            let n = (frames - done).min(self.left_in_tick as usize);
            let mut voices = core::array::from_fn(|i| self.channels[i].voice);
            mix(
                &mut voices,
                &self.module.samples,
                &self.pan,
                self.options.interpolate,
                &mut out[done * 2..(done + n) * 2],
            );
            for (channel, voice) in self.channels.iter_mut().zip(voices) {
                channel.voice = voice;
            }
            done += n;
            self.left_in_tick -= n as u32;
            if self.left_in_tick == 0 {
                self.end_tick();
            }
        }
        done
    }

    fn begin_tick(&mut self) {
        let tick = self.tick.min(255) as u8;
        if self.tick == 0 {
            self.start_row();
        } else {
            for channel in &mut self.channels {
                channel.tick(tick);
            }
        }
        for channel in &mut self.channels {
            channel.sync_voice(tick, self.rate);
        }
        let divisor = 2 * u32::from(self.tempo);
        self.tick_remainder += self.rate * 5;
        self.left_in_tick = (self.tick_remainder / divisor).max(1);
        self.tick_remainder %= divisor;
    }

    fn start_row(&mut self) {
        let mut flow = Flow::default();
        for (index, channel) in self.channels.iter_mut().enumerate() {
            let note = self.module.note(self.order, self.row, index);
            if channel.row(note, self.module, self.row, &mut flow) {
                self.unsupported += 1;
            }
        }
        if let Some(speed) = flow.speed {
            self.speed = speed;
        }
        if let Some(tempo) = flow.tempo {
            self.tempo = tempo;
        }
        self.delay = flow.pattern_delay.unwrap_or(0);
        self.flow = flow;
    }

    fn end_tick(&mut self) {
        self.tick += 1;
        if self.tick >= u16::from(self.speed) * (1 + u16::from(self.delay)) {
            self.tick = 0;
            self.advance_row();
        }
    }

    fn visited_bit(&self) -> (usize, u64) {
        let index = self.order * ROWS_PER_PATTERN + self.row;
        (index / 64, 1 << (index % 64))
    }

    fn mark_visited(&mut self) {
        let (word, bit) = self.visited_bit();
        if let Some(w) = self.visited.get_mut(word) {
            *w |= bit;
        }
    }

    fn clear_visited(&mut self, order: usize, row: usize) {
        let index = order * ROWS_PER_PATTERN + row;
        if let Some(w) = self.visited.get_mut(index / 64) {
            *w &= !(1 << (index % 64));
        }
    }

    fn already_visited(&self) -> bool {
        let (word, bit) = self.visited_bit();
        self.visited.get(word).is_none_or(|w| w & bit != 0)
    }

    /// One full play is over (the song wrapped or revisited a row).
    fn finish_play(&mut self) {
        self.plays += 1;
        if self.options.loops.is_some_and(|n| self.plays >= n) {
            self.finished = true;
        } else {
            self.visited = [0; VISITED_WORDS];
        }
    }

    fn advance_row(&mut self) {
        let flow = self.flow;
        let previous_row = self.row;
        let mut order = self.order;
        let mut row = self.row + 1;
        let mut backwards = false;
        if let Some(target) = flow.loop_back {
            row = target;
            backwards = true; // an `E6x` loop legitimately revisits rows
        } else if flow.jump.is_some() || flow.brk.is_some() {
            order = flow.jump.map_or(order + 1, usize::from);
            row = flow.brk.unwrap_or(0);
        }
        if row >= ROWS_PER_PATTERN {
            row = 0;
            order += 1;
        }
        let mut wrapped = false;
        if order >= self.module.orders.len() {
            order = usize::from(self.module.restart);
            wrapped = true;
        }
        self.order = order;
        self.row = row;
        if backwards {
            // The loop replays rows already marked; forget all but the first
            // so walking back over them is not mistaken for a repeated play.
            for replayed in row + 1..=previous_row {
                self.clear_visited(order, replayed);
            }
            self.mark_visited();
            return;
        }
        if wrapped || self.already_visited() {
            self.finish_play();
        }
        self.mark_visited();
    }
}
