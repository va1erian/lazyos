//! Per-channel state and the ProTracker effect commands.

use crate::mixer::Voice;
use crate::module::{Module, Note};
use crate::tables::{finetuned, semitones_up, MAX_PERIOD, MIN_PERIOD, SINE};

/// Song-flow requests a row's effects make; the player applies them when the
/// row ends.
#[derive(Clone, Copy, Debug, Default)]
pub struct Flow {
    /// `Bxx`: jump to this order.
    pub jump: Option<u8>,
    /// `Dxx`: continue at this row of the next (or jumped-to) order.
    pub brk: Option<usize>,
    /// `E6x`: loop back to this row.
    pub loop_back: Option<usize>,
    /// `Fxx` below 32.
    pub speed: Option<u8>,
    /// `Fxx` from 32.
    pub tempo: Option<u8>,
    /// `EEx`: repeat the row this many extra times.
    pub pattern_delay: Option<u8>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Channel {
    pub voice: Voice,
    sample: Option<usize>,
    period: u16,
    /// Tone-portamento destination.
    target: u16,
    volume: u8,
    finetune: i8,
    effect: u8,
    param: u8,
    slide_memory: u8,
    porta_memory: u8,
    vibrato_speed: u8,
    vibrato_depth: u8,
    vibrato_pos: u8,
    volslide_memory: u8,
    loop_row: usize,
    loop_count: u8,
    /// `EDx`: a note held back until that tick.
    delayed: Option<u16>,
}

fn clamp_period(period: i32) -> u16 {
    period.clamp(i32::from(MIN_PERIOD), i32::from(MAX_PERIOD)) as u16
}

/// Decimal row from a `Dxx` parameter; out-of-range rows restart the pattern.
fn break_row(param: u8) -> usize {
    let row = usize::from(param >> 4) * 10 + usize::from(param & 15);
    if row < 64 {
        row
    } else {
        0
    }
}

impl Channel {
    /// Current volume, `0..=64` (for UI meters).
    pub fn volume(&self) -> u8 {
        self.volume
    }

    fn trigger(&mut self, period: u16) {
        self.period = period;
        self.vibrato_pos = 0;
        self.voice.restart(self.sample);
    }

    /// Process the cell at the start of a row. Returns `true` when the cell
    /// used an effect this player does not implement.
    pub fn row(&mut self, note: Note, module: &Module, row: usize, flow: &mut Flow) -> bool {
        self.effect = note.effect;
        self.param = note.param;
        self.delayed = None;
        let (x, y) = (note.param >> 4, note.param & 15);

        if note.sample != 0 {
            let index = usize::from(note.sample) - 1;
            if let Some(sample) = module.samples.get(index) {
                self.sample = Some(index);
                self.volume = sample.volume;
                self.finetune = sample.finetune;
            }
        }
        if note.effect == 0xE && x == 5 {
            self.finetune = ((y << 4) as i8) >> 4;
        }
        if note.period != 0 {
            let period = finetuned(note.period, self.finetune);
            if matches!(note.effect, 3 | 5) {
                self.target = period;
            } else if note.effect == 0xE && x == 0xD && y > 0 {
                self.delayed = Some(period);
            } else {
                self.trigger(period);
            }
        }
        self.remember(note);
        self.first_tick_effect(note, row, flow)
    }

    /// Effects that reuse their last parameter when given zero.
    fn remember(&mut self, note: Note) {
        let p = note.param;
        match note.effect {
            1 | 2 if p != 0 => self.slide_memory = p,
            3 if p != 0 => self.porta_memory = p,
            4 => {
                if p >> 4 != 0 {
                    self.vibrato_speed = p >> 4;
                }
                if p & 15 != 0 {
                    self.vibrato_depth = p & 15;
                }
            }
            5 | 6 | 0xA if p != 0 => self.volslide_memory = p,
            _ => {}
        }
    }

    fn first_tick_effect(&mut self, note: Note, row: usize, flow: &mut Flow) -> bool {
        let p = note.param;
        let (x, y) = (p >> 4, p & 15);
        match note.effect {
            0 | 1 | 2 | 3 | 4 | 5 | 6 | 0xA => {}
            9 => self.voice.seek(usize::from(p) << 8),
            0xB => flow.jump = Some(p),
            0xC => self.volume = p.min(64),
            0xD => flow.brk = Some(break_row(p)),
            0xF => match p {
                0 => {}
                1..=31 => flow.speed = Some(p),
                _ => flow.tempo = Some(p),
            },
            0xE => return self.extended(x, y, row, flow),
            _ => return true,
        }
        false
    }

    fn extended(&mut self, sub: u8, y: u8, row: usize, flow: &mut Flow) -> bool {
        match sub {
            1 => self.period = clamp_period(i32::from(self.period) - i32::from(y)),
            2 => self.period = clamp_period(i32::from(self.period) + i32::from(y)),
            5 | 9 | 0xC | 0xD => {} // finetune applied above; the rest act on later ticks
            6 if y == 0 => self.loop_row = row,
            6 => {
                self.loop_count = if self.loop_count == 0 {
                    y
                } else {
                    self.loop_count - 1
                };
                if self.loop_count > 0 {
                    flow.loop_back = Some(self.loop_row);
                }
            }
            0xA => self.volume = (self.volume + y).min(64),
            0xB => self.volume = self.volume.saturating_sub(y),
            0xE => flow.pattern_delay = Some(y),
            _ => return true, // E0 filter, E3/E4/E7 waveforms, EF invert loop
        }
        if sub == 0xC && y == 0 {
            self.volume = 0;
        }
        false
    }

    fn volume_slide(&mut self) {
        let p = self.volslide_memory;
        if p >> 4 != 0 {
            self.volume = (self.volume + (p >> 4)).min(64);
        } else {
            self.volume = self.volume.saturating_sub(p & 15);
        }
    }

    fn tone_portamento(&mut self) {
        let step = i32::from(self.porta_memory);
        let (period, target) = (i32::from(self.period), i32::from(self.target));
        self.period = if target == 0 || period == target {
            self.period
        } else if period > target {
            (period - step).max(target) as u16
        } else {
            (period + step).min(target) as u16
        };
    }

    fn vibrato(&mut self) {
        self.vibrato_pos = self.vibrato_pos.wrapping_add(self.vibrato_speed) & 63;
    }

    /// Per-tick effects for ticks after the first of a row.
    pub fn tick(&mut self, tick: u8) {
        let (x, y) = (self.param >> 4, self.param & 15);
        match self.effect {
            1 => self.period = clamp_period(i32::from(self.period) - i32::from(self.slide_memory)),
            2 => self.period = clamp_period(i32::from(self.period) + i32::from(self.slide_memory)),
            3 => self.tone_portamento(),
            4 => self.vibrato(),
            5 => {
                self.tone_portamento();
                self.volume_slide();
            }
            6 => {
                self.vibrato();
                self.volume_slide();
            }
            0xA => self.volume_slide(),
            0xE => match x {
                9 if y != 0 && tick.is_multiple_of(y) => self.voice.restart(self.sample),
                0xC if tick == y => self.volume = 0,
                0xD if tick == y => {
                    if let Some(period) = self.delayed.take() {
                        self.trigger(period);
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }

    /// The period to mix at this tick: base period plus arpeggio or vibrato.
    fn mix_period(&self, tick: u8) -> u16 {
        match self.effect {
            0 if self.param != 0 => {
                let semitones = [0, self.param >> 4, self.param & 15][usize::from(tick % 3)];
                semitones_up(self.period, semitones)
            }
            4 | 6 => {
                let phase = self.vibrato_pos & 31;
                let delta =
                    (i32::from(SINE[usize::from(phase)]) * i32::from(self.vibrato_depth)) >> 7;
                let signed = if self.vibrato_pos & 32 != 0 {
                    -delta
                } else {
                    delta
                };
                clamp_period(i32::from(self.period) + signed)
            }
            _ => self.period,
        }
    }

    /// Push this tick's volume and pitch into the voice.
    pub fn sync_voice(&mut self, tick: u8, rate: u32) {
        self.voice.volume = self.volume;
        // Period 0 (no note played yet) makes the voice silent, not a division.
        self.voice.set_period(self.mix_period(tick), rate);
    }
}
