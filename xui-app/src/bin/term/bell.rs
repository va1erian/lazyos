//! The bell: what BEL does (`bell` setting: none, visual, sound, notify) and
//! how often. The rate limit and the tone are pure and host-tested; playing it
//! and posting the toast are best-effort and quiet when there is no sound
//! card or no shell to show it.

use std::f32::consts::TAU;

use lazyterm::settings::Bell;
use xui_app::platform::audio::{Audio, Params, PlaybackStream};
use xui_app::platform::notify::{self, Level, Notice};

/// At most one bell in this long (milliseconds): a program that rings in a
/// loop is not a siren.
pub const MIN_GAP_MS: u64 = 500;
/// How long the visual bell's flash lasts.
pub const FLASH_MS: u32 = 120;

/// The rate limit.
#[derive(Default)]
pub struct Limiter {
    last: Option<u64>,
}

impl Limiter {
    /// Whether a bell at `now` (milliseconds) is allowed; it counts if so.
    pub fn allow(&mut self, now: u64) -> bool {
        match self.last {
            Some(last) if now.saturating_sub(last) < MIN_GAP_MS => false,
            _ => {
                self.last = Some(now);
                true
            }
        }
    }
}

/// What to do for the bells that arrived: `None` when nothing (none, or rate
/// limited).
pub fn decide(setting: Bell, rang: u32, limiter: &mut Limiter, now: u64) -> Option<Bell> {
    (rang > 0 && setting != Bell::None && limiter.allow(now)).then_some(setting)
}

/// The bell tone: a short 880 Hz beep with a fade in and out, interleaved
/// stereo at `rate` Hz, at a polite level.
pub fn tone(rate: u32) -> Vec<i16> {
    let frames = (rate as usize * 110) / 1000;
    let fade = (rate as usize * 12) / 1000;
    let mut out = Vec::with_capacity(frames * 2);
    for i in 0..frames {
        let envelope = if i < fade {
            i as f32 / fade as f32
        } else if i + fade > frames {
            (frames - i) as f32 / fade as f32
        } else {
            1.0
        };
        let sample = (TAU * 880.0 * i as f32 / rate as f32).sin() * envelope * 0.18;
        let value = (sample * f32::from(i16::MAX)) as i16;
        out.extend([value, value]);
    }
    out
}

/// Play the tone through the system mixer (`audiod`), on a thread of its own:
/// opening the stream and waiting for it to drain must not hold the window.
/// Without a sound card or a mixer nothing happens.
pub fn play() {
    std::thread::spawn(|| {
        let Some(audio) = Audio::try_connect() else {
            return;
        };
        let rate = 22_050;
        let Ok(mut stream) = PlaybackStream::open(audio, Params::new(rate, 2)) else {
            return;
        };
        if stream.write(&tone(rate)).is_ok() {
            let _ = stream.finish();
        }
    });
}

/// An Info toast, for a bell while the window is not focused.
pub fn notify() {
    let _ = notify::post(&Notice::new(Level::Info, "Terminal", "Bell"));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn at_most_one_bell_in_half_a_second() {
        let mut l = Limiter::default();
        assert!(l.allow(1000));
        assert!(!l.allow(1100));
        assert!(!l.allow(1499));
        assert!(l.allow(1500));
        assert!(!l.allow(1500 + 499));
        assert!(l.allow(0 + 5000), "a later bell is fine");
        // A clock that went backwards does not wedge it.
        assert!(!l.allow(4000));
    }

    #[test]
    fn the_setting_and_the_count_decide() {
        let mut l = Limiter::default();
        assert_eq!(decide(Bell::Visual, 0, &mut l, 10_000), None, "no bell rang");
        assert_eq!(decide(Bell::None, 3, &mut l, 10_000), None, "disabled");
        assert_eq!(decide(Bell::Visual, 3, &mut l, 10_000), Some(Bell::Visual));
        assert_eq!(decide(Bell::Sound, 1, &mut l, 10_100), None, "rate limited");
        assert_eq!(decide(Bell::Notify, 1, &mut l, 10_600), Some(Bell::Notify));
        // A `none` bell does not use up the allowance.
        let mut l = Limiter::default();
        assert_eq!(decide(Bell::None, 1, &mut l, 0), None);
        assert_eq!(decide(Bell::Visual, 1, &mut l, 1), Some(Bell::Visual));
    }

    #[test]
    fn the_tone_is_short_quiet_and_clean_at_both_ends() {
        let t = tone(22_050);
        assert_eq!(t.len(), (22_050 * 110 / 1000) * 2, "stereo, 110 ms");
        assert_eq!(&t[..2], &[0, 0], "starts at silence");
        assert!(t[t.len() - 2..].iter().all(|v| v.abs() < 200), "ends near silence");
        let peak = t.iter().map(|v| v.unsigned_abs()).max().unwrap();
        assert!(peak > 3000 && peak < 8000, "audible but quiet: {peak}");
        assert!(t.chunks(2).all(|f| f[0] == f[1]), "both channels alike");
        // Crosses zero about 2 * 880 * 0.11 times.
        let crossings = t.chunks(2).map(|f| f[0]).collect::<Vec<_>>().windows(2).filter(|w| (w[0] < 0) != (w[1] < 0)).count();
        assert!((150..230).contains(&crossings), "{crossings}");
    }
}
