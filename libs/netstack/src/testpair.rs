//! Two stacks wired back to back, for the socket tests: `a` (10.0.0.1) and `b`
//! (10.0.0.2) share a /24, so a TCP or UDP exchange runs through the real
//! smoltcp on both ends with no gateway in the way.

use framering::fuzz::Mem;
use framering::{ring_bytes, Ring};

use crate::config::{Mode, StaticConfig};
use crate::device::RingDevice;
use crate::stack::Stack;

pub const A_IP: [u8; 4] = [10, 0, 0, 1];
pub const B_IP: [u8; 4] = [10, 0, 0, 2];
const SLOTS: u32 = 64;

pub struct Pair {
    _mem: Mem,
    pub a: Stack,
    pub b: Stack,
    pub now: i64,
}

fn static_mode(addr: [u8; 4]) -> Mode {
    Mode::Static(StaticConfig {
        addr,
        prefix_len: 24,
        gateway: None,
        dns: None,
    })
}

impl Pair {
    pub fn new() -> Pair {
        let one = ring_bytes(SLOTS);
        let mut mem = Mem::with_len(one * 2);
        let base = mem.base();
        // SAFETY: both rings lie inside `mem`, which the Pair keeps alive and
        // never moves.
        let (a_to_b, b_to_a) = unsafe {
            (
                Ring::create(base, one, SLOTS).expect("ring"),
                Ring::create(base.add(one), one, SLOTS).expect("ring"),
            )
        };
        let a_dev = RingDevice::new(b_to_a.consumer(), a_to_b.producer(), 1514);
        let b_dev = RingDevice::new(a_to_b.consumer(), b_to_a.producer(), 1514);
        Pair {
            _mem: mem,
            a: Stack::new(
                a_dev,
                [0x52, 0x54, 0, 0, 0, 1],
                0x1111,
                1000,
                &static_mode(A_IP),
            ),
            b: Stack::new(
                b_dev,
                [0x52, 0x54, 0, 0, 0, 2],
                0x2222,
                1000,
                &static_mode(B_IP),
            ),
            now: 1000,
        }
    }

    /// Advance the clock by `ms` and run both stacks until they go quiet.
    pub fn step(&mut self, ms: i64) {
        self.now += ms;
        for _ in 0..8 {
            self.a.poll(self.now);
            self.b.poll(self.now);
        }
    }

    /// Step in 10 ms ticks until `done` holds or `max_ms` pass.
    pub fn run_until(&mut self, max_ms: i64, mut done: impl FnMut(&mut Pair) -> bool) -> bool {
        let end = self.now + max_ms;
        while self.now < end {
            self.step(10);
            if done(self) {
                return true;
            }
        }
        false
    }
}

impl Default for Pair {
    fn default() -> Pair {
        Pair::new()
    }
}
