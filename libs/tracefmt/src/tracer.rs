//! The runtime half: the live mask, the per-channel budgets and the sink
//! contract. A runtime (kernel, no_std service, std app) owns one [`Tracer`]
//! in a `static` and implements [`Sink`] over its own output path.

use core::sync::atomic::{AtomicU64, Ordering};

use crate::budget::{Budget, Charge};
use crate::channel::MASK_BITS;
use crate::write::{self, LineBuf};
use crate::{Channel, Mask};

/// Where a finished line goes. One call per line, the whole line with its
/// newline: a sink must never split it, so another task's line cannot land
/// in the middle (`tray.json` once did).
pub trait Sink {
    fn write_line(&self, line: &[u8]);
}

/// Bytes a line may have (the rest is cut; a line of a trace is short).
pub const LINE_MAX: usize = 512;

const WORDS: usize = MASK_BITS / 64;

/// The mask and the budgets of one runtime.
pub struct Tracer {
    mask: [AtomicU64; WORDS],
    budgets: [Budget; Channel::ALL.len()],
}

impl Tracer {
    /// A tracer with the default mask ([`Mask::defaults`]'s value, computed
    /// at compile time so a `static` needs no init call).
    pub const fn new() -> Tracer {
        let mut budgets = [const { Budget::new(0, 0) }; Channel::ALL.len()];
        let mut index = 0;
        while index < budgets.len() {
            if let Some(rate) = Channel::ALL[index].rate() {
                budgets[index] = Budget::new(rate.lines, rate.window);
            }
            index += 1;
        }
        let mut words = [0u64; WORDS];
        let mut index = 0;
        while index < Channel::ALL.len() {
            if Channel::ALL[index].default_on() {
                words[index / 64] |= 1 << (index % 64);
            }
            index += 1;
        }
        Tracer {
            mask: [AtomicU64::new(words[0]), AtomicU64::new(words[1])],
            budgets,
        }
    }

    /// Start every channel's budget over.
    pub fn reset_budgets(&self) {
        self.budgets.iter().for_each(Budget::reset);
    }

    pub fn set_mask(&self, mask: Mask) {
        for (slot, word) in self.mask.iter().zip(mask.words()) {
            slot.store(word, Ordering::Relaxed);
        }
    }

    pub fn mask(&self) -> Mask {
        let mut words = [0u64; WORDS];
        for (word, slot) in words.iter_mut().zip(&self.mask) {
            *word = slot.load(Ordering::Relaxed);
        }
        Mask::from_words(words)
    }

    /// Whether `channel` prints (a relaxed load, no lock).
    pub fn enabled(&self, channel: Channel) -> bool {
        let index = channel.index();
        self.mask[index / 64].load(Ordering::Relaxed) >> (index % 64) & 1 == 1
    }

    /// Whether a line on `channel` may be written at time `now` (the
    /// channel's own clock, for its budget): false when the channel is masked
    /// or over budget. A permit that opens a new window after suppressed
    /// lines writes the `SUPPRESSED` notice to `sink` first. For a caller
    /// that builds a line of its own length; [`Tracer::emit`] is the common
    /// path.
    pub fn permit(&self, sink: &dyn Sink, now: u64, channel: Channel) -> bool {
        if !self.enabled(channel) {
            return false;
        }
        if channel.rate().is_some() {
            match self.budgets[channel.index()].charge(now) {
                Charge::Deny => return false,
                Charge::AllowAfter(count) => {
                    let mut notice = LineBuf::<96>::new();
                    let _ = write::suppressed(&mut notice, channel, count);
                    sink.write_line(notice.finish());
                }
                Charge::Allow => {}
            }
        }
        true
    }

    /// Emit one line on `channel` at time `now`: nothing when the channel is
    /// masked or over budget (see [`Tracer::permit`]), otherwise `build`
    /// writes the line into a buffer and the sink gets it whole. Returns
    /// whether a line was written.
    pub fn emit(
        &self,
        sink: &dyn Sink,
        now: u64,
        channel: Channel,
        build: impl FnOnce(&mut LineBuf<LINE_MAX>),
    ) -> bool {
        if !self.permit(sink, now, channel) {
            return false;
        }
        write_whole(sink, build);
        true
    }

    /// Like [`Tracer::emit`] but without the mask and the budget: for a
    /// protocol that must report whether or not tracing is on (`TEST:`).
    pub fn emit_always(&self, sink: &dyn Sink, build: impl FnOnce(&mut LineBuf<LINE_MAX>)) {
        write_whole(sink, build);
    }
}

fn write_whole(sink: &dyn Sink, build: impl FnOnce(&mut LineBuf<LINE_MAX>)) {
    let mut line = LineBuf::<LINE_MAX>::new();
    build(&mut line);
    sink.write_line(line.finish());
}

impl Default for Tracer {
    fn default() -> Self {
        Self::new()
    }
}
