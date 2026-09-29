//! The `soak=N` self-test (issue #169): drive request/reply cycles through the
//! serve loop and assert the daemon's bump heap did not grow across them.

use alloc::format;
use alloc::vec::Vec;
use user::messenger::{self, topics_client};
use user::sys;

use super::broker::Broker;

/// Self-soak cycles when the manifest asks for `soak` without a count.
const SOAK_DEFAULT_CYCLES: u64 = 4096;
/// Largest accepted `soak=N`, so a typo cannot run for hours.
const SOAK_MAX_CYCLES: u64 = 100_000;
/// Heap growth budget per soak cycle. The fixed 16 KiB recv-buffer leak this
/// guards against (issue #169) cost far more; the remaining ~100 bytes/cycle
/// is the encoded request/reply of the self-call itself.
const SOAK_BYTES_PER_CYCLE: u64 = 256;
/// Fixed slack on the soak's heap-growth budget, across allocator chunk
/// granularity and the other services' boot traffic during the soak.
const SOAK_BYTES_SLACK: u64 = 512 * 1024;
/// How often the daemon wakes to re-check the soak's finish conditions once
/// its cycles are done (PIT ticks).
pub(super) const SOAK_IDLE_TICKS: u64 = 5;
/// Absolute-tick cap on waiting for the service topics/subs to appear after
/// the soak's cycles (PIT ticks, 100 Hz).
const SOAK_TOPICS_TICKS: u64 = 3000;

/// The `soak=N` self-test state (issue #169): one request in flight at a
/// time, each an ordinary call the main loop serves.
pub(super) struct Soak {
    /// Pre-encoded `PING` request, reused every cycle.
    pub(super) request: libmessenger::Parcel,
    /// Reused `CALL_AWAIT` buffer.
    pub(super) reply_buffer: Vec<u8>,
    /// Cycles requested.
    pub(super) cycles_total: u64,
    /// Cycles still to run.
    pub(super) cycles_left: u64,
    /// Transaction of the self-call currently being served.
    pub(super) txn: Option<u64>,
    /// Heap break before the soak began.
    baseline: u64,
    /// Do not wait past this tick for the real service topics to appear.
    hard_tick: u64,
    /// A cycle failed; report `FAIL` even if the heap stayed flat.
    failed: bool,
}

impl Soak {
    /// Start a soak of `cycles` request/reply cycles and record the baseline.
    pub(super) fn start(cycles: u64) -> Soak {
        Soak {
            request: topics_client::ping_request(),
            reply_buffer: alloc::vec![0u8; messenger::DEFAULT_BUFFER],
            cycles_total: cycles,
            cycles_left: cycles,
            txn: None,
            baseline: sys::sbrk(0),
            hard_tick: sys::clock() + SOAK_TOPICS_TICKS,
            failed: false,
        }
    }

    /// Abandon the soak (a failed cycle).
    pub(super) fn fail(&mut self) {
        self.failed = true;
        self.cycles_left = 0;
        self.txn = None;
    }

    /// Whether the soak can report: its cycles are done (or failed) and either
    /// the real service topics/subscriptions are visible or the wait cap
    /// passed.
    pub(super) fn done(&self, broker: &Broker) -> bool {
        if self.cycles_left > 0 {
            return false;
        }
        if self.failed {
            return true;
        }
        let (topics, subs) = broker.counts();
        (topics >= 2 && subs >= 1) || sys::clock() >= self.hard_tick
    }
}

/// Print the soak's memory verdict and the central broker's real counts.
pub(super) fn finish_soak(broker: &Broker, soak: &Soak) {
    let growth = sys::sbrk(0).saturating_sub(soak.baseline);
    let budget = soak
        .cycles_total
        .saturating_mul(SOAK_BYTES_PER_CYCLE)
        .saturating_add(SOAK_BYTES_SLACK);
    if !soak.failed && growth <= budget {
        sys::write_str(&format!(
            "MSGRD:SOAK:PASS cycles={} bytes={growth}\n",
            soak.cycles_total
        ));
    } else {
        sys::write_str(&format!(
            "MSGRD:SOAK:FAIL cycles={} left={} bytes={growth} failed={}\n",
            soak.cycles_total, soak.cycles_left, soak.failed as u8
        ));
    }
    let (topics, subs) = broker.counts();
    if topics >= 2 && subs >= 1 {
        sys::write_str(&format!("MSGRD:TOPICS:PASS topics={topics} subs={subs}\n"));
    } else {
        sys::write_str(&format!("MSGRD:TOPICS:FAIL topics={topics} subs={subs}\n"));
    }
}

/// The requested self-soak cycle count: `soak=N` in the service arguments, or
/// `None` when the supervisor did not ask for one.
pub(super) fn soak_cycles() -> Option<u64> {
    let mut buffer = [0u8; 128];
    let len = sys::service_args(&mut buffer).min(buffer.len());
    let text = core::str::from_utf8(&buffer[..len]).unwrap_or("");
    for part in text.split_whitespace() {
        let Some(value) = part.strip_prefix("soak=") else {
            continue;
        };
        let cycles = value.parse::<u64>().unwrap_or(SOAK_DEFAULT_CYCLES);
        return Some(cycles.clamp(1, SOAK_MAX_CYCLES));
    }
    None
}

/// `Endpoint::await_reply` with a caller-owned buffer. The public method
/// allocates a fresh 16 KiB buffer per call, which long-lived loops must not
/// do; the soak uses this one so the daemon's own cycles stay flat too.
pub(super) fn await_reply_with(txn: u64, buffer: &mut [u8]) -> messenger::Result<()> {
    let args = messenger::MsgArgs {
        txn_id: txn,
        buf_ptr: buffer.as_mut_ptr() as u64,
        buf_cap: buffer.len() as u64,
        ..messenger::MsgArgs::default()
    };
    let mut result = messenger::MsgResult::default();
    let code = sys::messenger(
        messenger::op::CALL_AWAIT,
        &args as *const messenger::MsgArgs as u64,
        &mut result as *mut messenger::MsgResult as u64,
    );
    if code < 0 {
        return Err(messenger::Error::Errno(code));
    }
    Ok(())
}
