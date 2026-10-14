//! Hooks for the kernel test suite (`tests/serial_suite.rs`).

use super::*;

/// Test builds: send everything to the absent COM4 (`0x2E8`) instead of the
/// real port, at most `cap` bytes per drain call (a slow consumer), until the
/// returned guard is dropped. What was queued is discarded on both ends.
/// Nothing printed meanwhile reaches the real port: do not print from the
/// test body.
#[cfg(lazyos_tests)]
pub struct Diverted {
    saved: Option<(u16, usize)>,
}

#[cfg(lazyos_tests)]
pub fn divert_for_test(cap: usize) -> Diverted {
    const COM4: u16 = 0x2E8;
    let saved = SERIAL1
        .lock()
        .as_mut()
        .map(|port| {
            port.tx.drain_all();
            port.tx.divert(COM4, cap)
        });
    Diverted { saved }
}

#[cfg(lazyos_tests)]
impl Drop for Diverted {
    fn drop(&mut self) {
        if let (Some((base, cap)), Some(port)) = (self.saved, SERIAL1.lock().as_mut()) {
            port.tx.divert(base, cap);
        }
    }
}

/// Drain rounds [`mirror`] ran (a killed caller runs none).
#[cfg(lazyos_tests)]
pub static MIRROR_ROUNDS: AtomicU64 = AtomicU64::new(0);

/// Change the bytes one drain call may send on a diverted port.
#[cfg(lazyos_tests)]
pub fn set_cap_for_test(cap: usize) {
    if let Some(port) = SERIAL1.lock().as_mut() {
        port.tx.set_cap(cap);
    }
}

/// Test hook: bytes queued and sent so far, to judge a run.
#[cfg(lazyos_tests)]
pub fn counters_for_test() -> (usize, usize) {
    SERIAL1
        .lock()
        .as_ref()
        .map_or((0, 0), |port| (port.tx.queued(), port.tx.drained()))
}
