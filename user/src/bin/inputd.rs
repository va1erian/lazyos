//! `inputd` (`/system/bin/inputd`): the input policy service (`docs/input-plan.md`).
//!
//! The kernel raw event bus carries physical, HID-coded key edges and nothing
//! else. `inputd` is the only task holding `CAP_INPUT_RAW` and owns everything
//! that used to be hard-wired in the kernel: the keymap (compiled-in US and FR,
//! chosen by `confd` key `sys/input/layout`), modifier and lock state, key
//! repeat, hotkeys and the resync after a `Dropped` marker. It also owns the
//! one cursor every pointing device moves (`docs/usb-hid-plan.md`), which it
//! reports to the compositor alone. (Not `keyd`: that
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

use inputmap::{Output, PointerOut};
use user::messenger::input as api;
use user::messenger::{self, errno, registry, wait, Error};
use user::sys;

#[path = "inputd/clientcalls.rs"]
mod clientcalls;
#[path = "inputd/config.rs"]
mod config;
#[path = "inputd/console.rs"]
mod console;
#[path = "inputd/delivery.rs"]
mod delivery;
#[path = "inputd/grants.rs"]
mod grants;
#[path = "inputd/hub.rs"]
mod hub;
#[path = "inputd/keypages.rs"]
mod keypages;
#[path = "inputd/pointer.rs"]
mod pointer;
#[path = "inputd/settle.rs"]
mod settle;
#[path = "inputd/source.rs"]
mod source;
#[path = "inputd/trace.rs"]
mod trace;

use source::Item;

/// Longest park while nothing is due (PIT ticks, 100 Hz): the raw bus and the
/// service endpoint wake the loop themselves (the kernel's `wait` op), so this
/// only paces the `confd` layout watch, which is a pull subscription.
const IDLE_TICKS: u64 = 50;
/// Park while a client's backlog waits for room (it has no doorbell).
const BACKLOG_TICKS: u64 = 2;

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
    // One endpoint serves both interfaces. The loop parks on it and on the raw
    // bus together (`wait_any`), so a request is answered and a key or a
    // pointer move handled the moment it arrives (docs/performance-plan.md
    // P1.3).
    let (published, server) = messenger::create_pair().map_err(|_| "no service channel")?;
    let interfaces = [api::INTERFACE, api::SHELL_INTERFACE];
    registry::register(api::NAME, &published, &interfaces, 0)
        .map_err(|_| "cannot register os.lazy.input.v1")?;
    registry::register(api::SHELL_NAME, &published, &interfaces, 0)
        .map_err(|_| "cannot register os.lazy.input.shell.v1")?;
    // Serving: the autostart waits for this before opening app windows, so an
    // app's one-shot input session open cannot race the registration above
    // (init.Ready, P7.3).
    user::messenger::services::init::notify_ready();
    let mut hub = hub::Hub::new(config::default_layout());
    let mut config = config::Config::new();
    let mut trace = trace::Trace::from_args();
    let mut outputs: Vec<Output> = Vec::new();
    let mut pointer_outputs: Vec<PointerOut> = Vec::new();
    let mut buffer = alloc::vec![0u8; messenger::DEFAULT_BUFFER];
    sys::write_str(&format!(
        "INPUTD:READY layout={} interfaces={:#x},{:#x}
",
        hub.engine.layout().name(),
        api::INTERFACE,
        api::SHELL_INTERFACE
    ));

    loop {
        if let Some(layout) = config.poll() {
            if layout != hub.engine.layout() {
                hub.set_layout(layout);
                trace.layout(layout);
            }
        }
        let now = sys::clock();
        source.drain(|item| match item {
            Item::Key(raw) => hub.engine.feed(raw, now, &mut outputs),
            Item::Pointer(raw) => hub.pointer.engine.apply(raw, &mut pointer_outputs),
            Item::Dropped { ts_ns, seq, lost } => {
                trace::dropped(seq, lost);
                hub.engine.resync(ts_ns, seq, &mut outputs);
                hub.pointer.engine.resync(ts_ns, seq, &mut pointer_outputs);
            }
        });
        // One coalesced move per drain, after the edges it carried.
        hub.pointer.engine.flush(&mut pointer_outputs);
        trace.pointer(&pointer_outputs);
        hub.deliver_pointer(&pointer_outputs);
        pointer_outputs.clear();
        hub.engine.tick(now, &mut outputs);
        trace.outputs(&outputs);
        // Backlogs first, so this pass's keys queue behind older ones.
        hub.flush();
        hub.deliver_keys(&mut outputs, now);
        // The polled view follows the events just sent (I3).
        hub.publish_key_pages();
        // Evidence lines last, a bounded chunk at a time: the bus is never
        // left waiting on the serial port (issue #400).
        trace.flush();

        let idle = if hub.backlogged() {
            BACKLOG_TICKS
        } else {
            IDLE_TICKS
        };
        let wake = hub
            .engine
            .next_due()
            .into_iter()
            .chain(hub.keys_due())
            .fold(now + idle, u64::min)
            .max(now + 1);
        // Trace lines waiting: only look for work, never park (a `trace=1`
        // debug image), so they drain at the serial port's own pace.
        let wake = if trace.pending() { now } else { wake };
        let ready = match wait::wait_any(&[server], wait::WAIT_RAW_INPUT, Some(wake)) {
            Ok(mask) => mask,
            Err(Error::Errno(code)) if code == -errno::ETIMEDOUT => 0,
            // Never spin on a refused wait: fall back to a timed receive.
            Err(_) => 1,
        };
        if ready & 1 == 0 {
            continue;
        }
        // Ready means queued: this returns at once (the deadline only bounds
        // the fallback after a refused wait).
        match server.recv_with(&mut buffer, Some(wake)) {
            Ok(message) => {
                let reply = hub
                    .handle(&message)
                    .unwrap_or_else(|error| hub::Hub::error_reply(&message, error));
                if let Some(txn) = message.txn {
                    // A caller that gave up is not our problem.
                    let _ = server.reply(txn, &reply);
                }
            }
            Err(Error::Errno(code)) if code == -errno::ETIMEDOUT => {}
            Err(_) => {}
        }
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    sys::exit(1)
}
