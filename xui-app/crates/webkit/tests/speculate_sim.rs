//! The picture shown ahead of the page (`speculate.rs`), simulated: seeded pages that follow the wheel
//! late (some over a second, as a heavy page loading does), in bursts, and not at all past their end,
//! answering every turn before the frame that shows it.

use xui_webkit::speculate::{Speculation, LINE_PX, LOST_MS, MAX_AHEAD};

/// A tiny deterministic generator (no dependency for a test).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// A page with `limit` pixels of scroll that handles each wheel turn `latency` ms after it was
/// sent, one at a time, and answers it with its offset (an update with it follows).
struct Page {
    offset: i32,
    limit: i32,
    queue: Vec<(u64, i32, u64)>,
    follows: bool,
    /// The page was asked for more than it had (its end, or its top): no model can know that.
    clamped: bool,
}

impl Page {
    fn send(&mut self, now: u64, ticks: i32, latency: u64, seq: u64) {
        self.queue.push((now + latency, ticks, seq));
    }

    /// The answers the page gives by `now`: `(when, turn, offset)`. A page that does not follow answers
    /// too (it handled the turn and stayed).
    fn run(&mut self, now: u64) -> Vec<(u64, u64, i32)> {
        let mut answers = Vec::new();
        while let Some(&(due, ticks, seq)) = self.queue.first() {
            if due > now {
                break;
            }
            self.queue.remove(0);
            if self.follows {
                let wanted = self.offset - ticks.clamp(-8, 8) * LINE_PX;
                let next = wanted.clamp(0, self.limit);
                self.clamped |= next != wanted;
                self.offset = next;
            }
            answers.push((due, seq, self.offset));
        }
        answers
    }
}

#[test]
fn simulated_pages_the_picture_never_strays_and_comes_back_to_zero() {
    for seed in 1..=400u64 {
        let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
        let limit = [0, 300, 4000, 100_000][rng.below(4) as usize];
        let mut page = Page {
            offset: if limit == 0 {
                0
            } else {
                (rng.below(limit as u64 + 1) as i32 / LINE_PX) * LINE_PX
            },
            limit,
            queue: Vec::new(),
            follows: rng.below(8) != 0,
            clamped: false,
        };
        let mut s = Speculation::default();
        s.report(page.offset, 0);
        // Busy pages too: a heavy page's script holds the wheel for over a second.
        let busy = rng.below(4) == 0;
        let latency = 20 + rng.below(if busy { 2500 } else { 400 });
        let mut now = 0u64;
        let mut seq = 0u64;
        let mut answers = Vec::new();
        let wheel_until = 200 + rng.below(3000);
        while now < wheel_until + latency + LOST_MS + 2000 {
            now += 1 + rng.below(40);
            if now < wheel_until && rng.below(3) == 0 {
                let ticks = if rng.below(5) == 0 { 1 } else { -1 } * (1 + rng.below(3) as i32);
                seq += 1;
                page.send(now, ticks, latency, seq);
                s.wheel(ticks, now, seq);
            }
            // The answer comes first, the frame that shows it a little later.
            for (due, turn, offset) in page.run(now) {
                s.answered(turn, offset, now);
                answers.push((due + 1 + rng.below(30), offset));
            }
            while answers.first().is_some_and(|(due, _)| *due <= now) {
                let (_, offset) = answers.remove(0);
                s.report(offset, now);
            }
            s.tick(now);
            assert!(
                s.shift().abs() <= MAX_AHEAD,
                "seed {seed}: {} ahead",
                s.shift()
            );
            if let Some(top) = s.actual() {
                assert!(
                    top + s.shift() >= 0,
                    "seed {seed}: ahead of the top of the page"
                );
            }
        }
        assert_eq!(
            s.shift(),
            0,
            "seed {seed}: still ahead after everything went quiet"
        );
        if page.follows && !page.clamped {
            // A page that did every notch in full, however late: what was shown ahead was always given back.
            assert_eq!(
                s.counters().snaps,
                0,
                "seed {seed}: the picture jumped back on a page that followed (latency {latency}, wheel until {wheel_until}, {:?}, offset {})",
                s.counters(),
                page.offset
            );
        }
    }
}
