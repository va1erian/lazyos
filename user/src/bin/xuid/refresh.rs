//! The screen's refresh rate, for `GetOutput` (`idl/display.midl`): clients pace their presents to it.
//!
//! The kernel's display grant knows no refresh rate. On the Kaby Lake box `gpud` measures the adopted
//! pipe's from its frame counter (`GetBeam.measured_millihz`, docs/kabylake-gpu-plan.md); anywhere else
//! (QEMU, a GPU nobody drives) there is no vblank to follow and 60 Hz is the answer.
//!
//! `gpud` may start after the compositor (`devd` starts it when it finds the device) and only knows the
//! rate a second after it starts, so a miss is asked again at later `GetOutput`s, a few times at most.
//! The call is bounded: the compositor never waits long on another service.

use user::messenger::gpu;
use user::sys;
use user::trace::{self, Channel};

/// The rate when nobody measured one.
pub(super) const DEFAULT_MILLIHZ: u32 = 60_000;
/// Rates outside these bounds are a misread counter, not a screen.
const MIN_MILLIHZ: u32 = 20_000;
const MAX_MILLIHZ: u32 = 250_000;
/// `GetOutput`s that still ask `gpud` before the default is kept for good.
const TRIES: u8 = 8;
/// How long one question may take, in 100 Hz ticks.
const DEADLINE_TICKS: u64 = 5;

/// The rate answered so far, and how many more times a miss asks again.
pub(super) struct Refresh {
    known: Option<u32>,
    tries_left: u8,
}

impl Default for Refresh {
    fn default() -> Refresh {
        Refresh {
            known: None,
            tries_left: TRIES,
        }
    }
}

impl Refresh {
    /// The refresh rate in millihertz, asking `gpud` while it is unknown and tries are left.
    pub(super) fn millihz(&mut self) -> u32 {
        if let Some(known) = self.known {
            return known;
        }
        if self.tries_left == 0 {
            return DEFAULT_MILLIHZ;
        }
        self.tries_left -= 1;
        match measured() {
            Some(millihz) => {
                trace::line!(Channel::Xuid, "REFRESH source=gpud millihz={millihz}");
                self.known = Some(millihz);
                millihz
            }
            None => {
                if self.tries_left == 0 {
                    trace::line!(
                        Channel::Xuid,
                        "REFRESH source=default millihz={DEFAULT_MILLIHZ}"
                    );
                }
                DEFAULT_MILLIHZ
            }
        }
    }
}

/// `gpud`'s measured rate, if it runs, answers in time and measured a sane one.
fn measured() -> Option<u32> {
    let client = gpu::Client::connect().ok()?;
    let beam = client.beam_by(Some(sys::clock() + DEADLINE_TICKS));
    client.close();
    sane(beam.ok()?.measured_millihz)
}

/// `millihz` when it can be a screen's refresh rate.
pub(super) fn sane(millihz: u32) -> Option<u32> {
    (MIN_MILLIHZ..=MAX_MILLIHZ)
        .contains(&millihz)
        .then_some(millihz)
}
