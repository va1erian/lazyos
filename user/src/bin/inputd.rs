//! `inputd` (`INPUTD.ELF`): the input policy service (`docs/input-plan.md`).
//!
//! The kernel raw event bus carries physical, HID-coded key edges and nothing
//! else. `inputd` is the only task holding `CAP_INPUT_RAW` and owns everything
//! that used to be hard-wired in the kernel: the keymap (compiled-in US and FR,
//! chosen by `confd` key `sys/input/layout`), modifier and lock state, key
//! repeat, hotkeys and the resync after a `Dropped` marker. (Not `keyd`: that
//! is the secrets and crypto service.)
//!
//! Boot lines: `INPUTD:READY layout=<name>`; with `trace=1` every decoded
//! event is echoed as `INPUTD:KEY`/`INPUTD:TEXT` for scripted verification.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::vec::Vec;
use core::panic::PanicInfo;

use inputmap::{Engine, Output};
use user::messenger;
use user::sys;

#[path = "inputd/config.rs"]
mod config;
#[path = "inputd/source.rs"]
mod source;
#[path = "inputd/trace.rs"]
mod trace;

use source::Item;

/// Longest park between raw-bus polls (PIT ticks, 100 Hz): bounds typing
/// latency at 20 ms without a per-event wakeup (the bus has no doorbell yet).
const POLL_TICKS: u64 = 2;

#[no_mangle]
pub extern "C" fn _start() -> ! {
    sys::write_str("inputd: input policy service\n");
    match run() {
        Ok(()) => sys::exit(0),
        Err(what) => {
            sys::write_str(&format!("inputd: fatal: {what}\n"));
            sys::exit(1)
        }
    }
}

fn run() -> Result<(), &'static str> {
    let mut source = source::Source::open().map_err(|_| "cannot open the raw input bus")?;
    // A private channel used only as a timed wait: `recv_with` on it parks
    // the task until the deadline. The far end must stay open, or the receive
    // would fail instead of waiting.
    let (_far, sleeper) = messenger::create_pair().map_err(|_| "no timer channel")?;
    let mut engine = Engine::new(config::default_layout());
    let mut config = config::Config::new();
    let trace = trace::Trace::from_args();
    let mut outputs: Vec<Output> = Vec::new();
    let mut wait_buffer = alloc::vec![0u8; 64];
    sys::write_str(&format!("INPUTD:READY layout={}\n", engine.layout().name()));

    loop {
        if let Some(layout) = config.poll() {
            if layout != engine.layout() {
                engine.set_layout(layout);
                trace.layout(layout);
            }
        }
        let now = sys::clock();
        source.drain(|item| match item {
            Item::Key(raw) => engine.feed(raw, now, &mut outputs),
            Item::Dropped { ts_ns, seq } => engine.resync(ts_ns, seq, &mut outputs),
        });
        engine.tick(now, &mut outputs);
        trace.outputs(&outputs);
        outputs.clear();

        let wake = engine
            .next_due()
            .map_or(now + POLL_TICKS, |due| due.min(now + POLL_TICKS))
            .max(now + 1);
        // The wait ends by timeout every time; anything else is not a reason
        // to spin, so it also just loops.
        let _ = sleeper.recv_with(&mut wait_buffer, Some(wake));
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    sys::exit(1)
}
