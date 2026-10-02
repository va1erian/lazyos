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
use user::messenger::{self, errno, registry, Error};
use user::sys;

#[path = "inputd/config.rs"]
mod config;
#[path = "inputd/hub.rs"]
mod hub;
#[path = "inputd/pointer.rs"]
mod pointer;
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
    // One endpoint serves both interfaces; the receive loop also paces the raw
    // bus polling (the bus has no doorbell yet), so a request is answered the
    // moment it arrives and a key within `POLL_TICKS`.
    let (published, server) = messenger::create_pair().map_err(|_| "no service channel")?;
    let interfaces = [api::INTERFACE, api::SHELL_INTERFACE];
    registry::register(api::NAME, &published, &interfaces, 0)
        .map_err(|_| "cannot register os.lazy.input.v1")?;
    registry::register(api::SHELL_NAME, &published, &interfaces, 0)
        .map_err(|_| "cannot register os.lazy.input.shell.v1")?;
    let mut hub = hub::Hub::new(config::default_layout());
    let mut config = config::Config::new();
    let trace = trace::Trace::from_args();
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
            Item::Dropped { ts_ns, seq } => {
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
        hub.deliver(&outputs);
        outputs.clear();

        let wake = hub
            .engine
            .next_due()
            .map_or(now + POLL_TICKS, |due| due.min(now + POLL_TICKS))
            .max(now + 1);
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
