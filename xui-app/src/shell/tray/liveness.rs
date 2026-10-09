//! Liveness and event delivery on the items' channels (docs/tray-plan.md
//! section 3): the shell `Ping`s every channel about once a second, as
//! `xuid` does to its windows, and a send that fails with `EPIPE` means the
//! app died (or closed its end), so its custom item goes. A resident app's
//! default item stays until `init` reports it stopped.

use libmessenger::{flags, Header, Parcel, VERSION};
use messenger_generated::os_lazy_shell_tray_events_v1 as events;

use super::super::ctx::Ctx;
use crate::sys::{self, errno};

/// Ticks (100 Hz) between pings: about once a second, the cadence `xuid`
/// pings its windows at. A dead app's item lingers at most that long; pinging
/// on every 30 ms desktop tick would cost ~33 sends per item per second to
/// shave a second nobody notices.
const PING_TICKS: u64 = 100;

thread_local! {
    static NEXT_PING: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Send one oneway event on `channel`.
pub fn send(channel: u64, method: u32, body: Vec<u8>) -> Result<(), i64> {
    let parcel = Parcel {
        header: Header {
            version: VERSION,
            flags: flags::ONE_WAY,
            interface_id: events::INTERFACE_ID,
            method,
            txn_id: 0,
            reply_to: 0,
            deadline_ns: 0,
        },
        body,
        objects: Vec::new(),
    };
    sys::msg_send(channel, &parcel)
}

/// Whether `app`'s item channel still has a live peer: one `Ping` now.
pub fn alive(ctx: &Ctx, app: &str) -> bool {
    let Some(channel) = ctx.tray.channel(app) else {
        return false;
    };
    match send(channel, events::METHOD_PING, Vec::new()) {
        Ok(()) => true,
        Err(code) => code == -errno::EAGAIN,
    }
}

/// Drop the custom item of `app` whose channel broke.
pub fn gone(ctx: &Ctx, app: &str) {
    ctx.tray.model.borrow_mut().clear(app);
    ctx.tray.drop_channel(app);
    println!("SHELL:TRAY:CLEAR app={app} why=gone");
}

/// Ping every channel when due; `true` when an item went away.
pub fn pump(ctx: &Ctx) -> bool {
    let now = sys::clock_ticks();
    if NEXT_PING.with(|next| next.get()) > now {
        return false;
    }
    NEXT_PING.with(|next| next.set(now.saturating_add(PING_TICKS)));
    let channels: Vec<(String, u64, u32)> = ctx.tray.channels.borrow().clone();
    let mut dropped = false;
    for (app, channel, _) in channels {
        match send(channel, events::METHOD_PING, Vec::new()) {
            // A full queue is a slow app, not a dead one.
            Ok(()) => {}
            Err(code) if code == -errno::EAGAIN => {}
            Err(_) => {
                gone(ctx, &app);
                dropped = true;
            }
        }
    }
    dropped
}
