//! A [`Sink`] on the system mixer (`audiod`), and the bookkeeping that makes
//! a pause lossless.
//!
//! The mixer cannot pause a stream: `Stop` discards what is queued and
//! restarts frame numbering. So [`MixerSink`] closes the stream on pause and
//! keeps its own copy of what it queued but the mixer has not consumed yet,
//! and queues that again on resume. It resumes from a freshly refreshed
//! *consumed* count, not from `Position`: a recording of a pause on QEMU's
//! virtio-sound shows the frames between the two do reach the speakers
//! (resuming from `Position` repeated 64 ms), and a stale consumed count
//! repeated 20 ms. Resumed from a fresh one, the music joins up.
//!
//! The bookkeeping runs against [`Stream`], which `PlaybackStream` implements
//! and the tests fake, so it is checked on the host.

use std::collections::VecDeque;

use audioclient::{Error, Params, PlaybackStream, Transport};
use xui_app::platform::audio::{Audio, UNITY_GAIN};

use super::sink::Sink;

/// Bytes per period asked of the mixer; it widens or caps it to fit its ring.
const PERIOD_BYTES: u32 = 16 * 1024;

/// What the sink needs from a mixer stream (interleaved stereo `i16`).
pub trait Stream {
    /// Frames the ring holds.
    fn ring_frames(&self) -> u64;
    /// Frames written to this stream so far.
    fn written(&self) -> u64;
    /// Frames that can be written now (also refreshes what was consumed).
    fn free_frames(&mut self) -> Result<u64, Error>;
    /// Write what fits; the stream starts itself once its ring is full.
    fn try_write(&mut self, samples: &[i16]) -> Result<usize, Error>;
    fn start(&mut self) -> Result<(), Error>;
    /// 16.16 gain.
    fn set_volume(&mut self, gain_q16: u32) -> Result<(), Error>;
    fn close(self: Box<Self>) -> Result<(), Error>;
}

impl<T: Transport> Stream for PlaybackStream<T> {
    fn ring_frames(&self) -> u64 {
        let grant = self.grant();
        u64::from(grant.period_bytes) * u64::from(grant.periods) / (2 * u64::from(grant.channels))
    }
    fn written(&self) -> u64 {
        PlaybackStream::written(self)
    }
    fn free_frames(&mut self) -> Result<u64, Error> {
        PlaybackStream::free_frames(self)
    }
    fn try_write(&mut self, samples: &[i16]) -> Result<usize, Error> {
        PlaybackStream::try_write(self, samples)
    }
    fn start(&mut self) -> Result<(), Error> {
        PlaybackStream::start(self)
    }
    fn set_volume(&mut self, gain_q16: u32) -> Result<(), Error> {
        PlaybackStream::set_volume(self, gain_q16)
    }
    fn close(self: Box<Self>) -> Result<(), Error> {
        PlaybackStream::close(*self)
    }
}

/// Opens a stream at a rate; `Ok(None)` when no mixer runs.
pub type Opener = Box<dyn Fn(u32) -> Result<Option<Box<dyn Stream>>, String>>;

/// The real mixer, over `xui_app::platform::audio`.
pub fn audiod() -> Opener {
    Box::new(|rate| {
        let Some(audio) = Audio::try_connect() else {
            return Ok(None);
        };
        let params = Params::new(rate, 2).period_bytes(PERIOD_BYTES);
        let stream = PlaybackStream::open(audio, params).map_err(failed("open"))?;
        if stream.channels() != 2 {
            return Err("audio open: the mixer did not grant stereo".to_owned());
        }
        Ok(Some(Box::new(stream) as Box<dyn Stream>))
    })
}

fn failed(what: &str) -> impl Fn(Error) -> String + '_ {
    move |error| format!("audio {what}: {error}")
}

/// A [`Sink`] on a mixer stream that survives pauses. Frame counts are in two
/// numberings: *totals* over the sink's life ([`Sink::played`],
/// [`Sink::written`]) and *stream* frames, which restart with every stream.
pub struct MixerSink {
    open: Opener,
    stream: Option<Box<dyn Stream>>,
    rate: u32,
    volume: u8,
    /// Total frames heard in streams closed by earlier pauses.
    base: u64,
    /// Stream frames the mixer has consumed, as of the last refresh.
    consumed: u64,
    /// The samples queued in the current stream after `consumed`.
    queued: VecDeque<i16>,
    /// Samples accepted but not yet in a stream (after a resume, a backlog
    /// larger than the new ring waits here).
    pending: VecDeque<i16>,
    /// Total frames accepted.
    written: u64,
    started: bool,
}

impl MixerSink {
    /// A sink on a fresh stream from `open`, or `None` when no mixer runs.
    pub fn open(open: Opener, rate: u32) -> Result<Option<MixerSink>, String> {
        let Some(stream) = open(rate)? else {
            return Ok(None);
        };
        Ok(Some(MixerSink {
            open,
            stream: Some(stream),
            rate,
            volume: 100,
            base: 0,
            consumed: 0,
            queued: VecDeque::new(),
            pending: VecDeque::new(),
            written: 0,
            started: false,
        }))
    }

    /// Move pending samples into the stream as far as they fit.
    fn drain_pending(&mut self) -> Result<(), String> {
        let Some(stream) = self.stream.as_mut() else {
            return Ok(());
        };
        if !self.pending.is_empty() {
            let samples = self.pending.make_contiguous();
            let frames = stream
                .try_write(&samples[..samples.len() & !1])
                .map_err(failed("write"))?;
            let moved: Vec<i16> = self.pending.drain(..frames * 2).collect();
            self.queued.extend(moved);
        }
        self.started |= stream.written() >= stream.ring_frames();
        Ok(())
    }

    /// Note what the mixer consumed, and forget those frames.
    fn refresh(&mut self) -> Result<u64, String> {
        let Some(stream) = self.stream.as_mut() else {
            return Ok(0);
        };
        let free = stream.free_frames().map_err(failed("commit"))?;
        let ring = stream.ring_frames();
        let written = stream.written();
        let consumed = written - (ring - free.min(ring)).min(written);
        let done = (consumed.saturating_sub(self.consumed) as usize * 2).min(self.queued.len());
        self.queued.drain(..done);
        self.consumed = consumed;
        Ok(free)
    }
}

impl Sink for MixerSink {
    fn rate(&self) -> u32 {
        self.rate
    }

    fn is_audio(&self) -> bool {
        true
    }

    fn free(&mut self) -> Result<usize, String> {
        self.drain_pending()?;
        let free = self.refresh()?;
        Ok(if self.pending.is_empty() { free as usize } else { 0 })
    }

    fn write(&mut self, samples: &[i16]) -> Result<usize, String> {
        if !self.pending.is_empty() {
            return Ok(0);
        }
        let Some(stream) = self.stream.as_mut() else {
            return Ok(0);
        };
        let frames = stream.try_write(samples).map_err(failed("write"))?;
        self.queued.extend(&samples[..frames * 2]);
        self.written += frames as u64;
        // The stream starts itself once a full ring is queued.
        self.started |= stream.written() >= stream.ring_frames();
        Ok(frames)
    }

    fn played(&self) -> u64 {
        self.base + self.consumed
    }

    fn written(&self) -> u64 {
        self.written
    }

    fn flush(&mut self) -> Result<(), String> {
        if let (false, Some(stream)) = (self.started, self.stream.as_mut()) {
            stream.start().map_err(failed("start"))?;
            self.started = true;
        }
        Ok(())
    }

    fn pause(&mut self) -> Result<(), String> {
        // The mixer has read on since the last refresh.
        self.refresh()?;
        let Some(stream) = self.stream.take() else {
            return Ok(());
        };
        // What the mixer never read goes back in front of anything pending.
        let mut requeue = std::mem::take(&mut self.queued);
        requeue.extend(self.pending.drain(..));
        self.pending = requeue;
        self.base += self.consumed;
        self.consumed = 0;
        self.started = false;
        stream.close().map_err(failed("close"))
    }

    fn resume(&mut self) -> Result<(), String> {
        if self.stream.is_some() {
            return Ok(());
        }
        let stream = (self.open)(self.rate)?.ok_or("audio: the mixer is gone")?;
        self.stream = Some(stream);
        self.set_volume(self.volume)?;
        self.drain_pending()
    }

    fn set_volume(&mut self, percent: u8) -> Result<(), String> {
        self.volume = percent.min(100);
        let gain = UNITY_GAIN * u32::from(self.volume) / 100;
        match self.stream.as_mut() {
            Some(stream) => stream.set_volume(gain).map_err(failed("volume")),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use super::*;

    /// A fake mixer stream: a ring of `ring` frames, read by the test in
    /// periods (`consume`). Every frame read reaches the speakers (`heard`),
    /// as a recording on QEMU shows; what was queued and not read is lost
    /// when the stream closes. A test checks the music came out whole.
    #[derive(Default)]
    struct Card {
        ring: u64,
        written: u64,
        consumed: u64,
        started: bool,
        open: bool,
        queued: VecDeque<i16>,
        heard: Vec<i16>,
    }

    impl Card {
        /// The mixer reads up to `frames`, if the stream runs.
        fn consume(&mut self, frames: u64) {
            if !(self.open && self.started) {
                return;
            }
            let frames = frames.min(self.written - self.consumed);
            self.consumed += frames;
            let samples: Vec<i16> = self.queued.drain(..frames as usize * 2).collect();
            self.heard.extend(samples);
        }
    }

    struct FakeStream(Rc<RefCell<Card>>);

    impl Stream for FakeStream {
        fn ring_frames(&self) -> u64 {
            self.0.borrow().ring
        }
        fn written(&self) -> u64 {
            self.0.borrow().written
        }
        fn free_frames(&mut self) -> Result<u64, Error> {
            let card = self.0.borrow();
            Ok(card.ring - (card.written - card.consumed))
        }
        fn try_write(&mut self, samples: &[i16]) -> Result<usize, Error> {
            let free = self.free_frames()? as usize;
            let mut card = self.0.borrow_mut();
            let frames = (samples.len() / 2).min(free);
            card.written += frames as u64;
            card.queued.extend(&samples[..frames * 2]);
            card.started |= card.written - card.consumed >= card.ring;
            Ok(frames)
        }
        fn start(&mut self) -> Result<(), Error> {
            self.0.borrow_mut().started = true;
            Ok(())
        }
        fn set_volume(&mut self, _: u32) -> Result<(), Error> {
            Ok(())
        }
        fn close(self: Box<Self>) -> Result<(), Error> {
            // What was queued and not read is gone.
            let mut card = self.0.borrow_mut();
            card.open = false;
            card.queued.clear();
            Ok(())
        }
    }

    /// A sink, its current card, and what earlier cards played.
    type Rig = (MixerSink, Rc<RefCell<Card>>, Rc<RefCell<Vec<i16>>>);

    /// A sink over fake cards; each (re)open starts a fresh card and moves
    /// what earlier cards played into `heard`.
    fn rig(ring: u64) -> Rig {
        let current = Rc::new(RefCell::new(Card::default()));
        let heard = Rc::new(RefCell::new(Vec::new()));
        let (slot, all) = (Rc::clone(&current), Rc::clone(&heard));
        let open: Opener = Box::new(move |_rate| {
            let old = std::mem::take(&mut *slot.borrow_mut());
            all.borrow_mut().extend(old.heard);
            *slot.borrow_mut() = Card {
                ring,
                open: true,
                ..Card::default()
            };
            Ok(Some(Box::new(FakeStream(Rc::clone(&slot))) as Box<dyn Stream>))
        });
        let sink = MixerSink::open(open, 22_050).unwrap().unwrap();
        (sink, current, heard)
    }

    /// Feed the sink a counting signal (frame n = n, both channels) as a deck
    /// would, `steps` times, with the mixer reading `period` frames each time.
    fn play(sink: &mut MixerSink, card: &Rc<RefCell<Card>>, next: &mut i16, steps: usize) {
        for _ in 0..steps {
            let free = sink.free().unwrap();
            let samples: Vec<i16> = (0..free)
                .flat_map(|i| {
                    let v = next.wrapping_add(i as i16);
                    [v, v]
                })
                .collect();
            let taken = sink.write(&samples).unwrap();
            *next = next.wrapping_add(taken as i16);
            card.borrow_mut().consume(300);
        }
    }

    fn heard_in_order(heard: &[i16]) {
        for (n, pair) in heard.chunks(2).enumerate() {
            assert_eq!(pair[0], n as i16, "frame {n} out of order");
        }
    }

    #[test]
    fn frames_play_in_order_and_are_counted() {
        let (mut sink, card, _) = rig(1000);
        let mut next = 0;
        play(&mut sink, &card, &mut next, 20);
        sink.free().unwrap(); // a deck asks before each fill
        let card = card.borrow();
        heard_in_order(&card.heard);
        assert_eq!(sink.played(), card.consumed);
        assert_eq!(sink.written(), u64::from(next as u16));
    }

    #[test]
    fn a_pause_resumes_where_the_mixer_stopped_reading() {
        let (mut sink, card, earlier) = rig(1000);
        let mut next = 0;
        for _ in 0..5 {
            play(&mut sink, &card, &mut next, 7);
            sink.pause().unwrap();
            assert_eq!(sink.free().unwrap(), 0, "paused");
            sink.resume().unwrap();
        }
        play(&mut sink, &card, &mut next, 30);
        let mut heard = earlier.borrow().clone();
        heard.extend(&card.borrow().heard);
        heard_in_order(&heard);
        assert!(heard.len() / 2 > 5000, "{}", heard.len() / 2);
        // Totals stay consistent across the pauses: played never runs ahead.
        assert!(sink.played() <= sink.written());
    }

    #[test]
    fn a_pause_before_the_stream_started_keeps_everything() {
        let (mut sink, card, earlier) = rig(1000);
        let half: Vec<i16> = (0..400).flat_map(|v| [v as i16, v as i16]).collect();
        assert_eq!(sink.write(&half).unwrap(), 400);
        sink.pause().unwrap();
        sink.resume().unwrap();
        let mut next = 400;
        play(&mut sink, &card, &mut next, 10);
        let mut heard = earlier.borrow().clone();
        heard.extend(&card.borrow().heard);
        heard_in_order(&heard);
        assert!(heard.len() / 2 >= 1000);
    }

    #[test]
    fn flush_starts_a_short_stream() {
        let (mut sink, card, _) = rig(1000);
        sink.write(&[1, 1, 2, 2]).unwrap();
        card.borrow_mut().consume(2);
        assert_eq!(card.borrow().consumed, 0, "not started yet");
        sink.flush().unwrap();
        card.borrow_mut().consume(2);
        assert_eq!(card.borrow().consumed, 2);
    }
}
