//! The radio program's loop: register both names, then wait on the two
//! service endpoints and the hardware's wake-up endpoint, and on every wake
//! pump the hardware, run the soft-MAC's timers and receive path, answer
//! requests, move data frames, send events and follow the link.

use alloc::format;
use alloc::string::String;
use alloc::vec;

use wifimac::{Radio, SoftMac};

use super::hw::{self, HwService};
use super::nic::NicService;
use crate::central;
use crate::messenger::net::{self as nic_api, wire as nic_wire};
use crate::messenger::{self, registry, wait, Endpoint, Error as MsgError};
use crate::sys;

/// Longest park with nothing timed to do: the hardware's endpoint and the
/// clients wake the loop sooner.
const IDLE_MS: u64 = 100;
/// Requests answered per endpoint per wake, so neither starves the other.
const REQUEST_BUDGET: usize = 16;

/// A radio that needs the loop's help: the endpoint whose messages mean "the
/// hardware has something" (an interrupt channel, the simulator's air card),
/// and the work to do on every wake (move frames between the hardware and
/// the radio's queues).
pub trait Hardware: Radio {
    /// The endpoint to wait on besides the two services, if any.
    fn wake_endpoint(&self) -> Option<Endpoint>;
    /// Service the hardware. An error ends the program (`init` restarts it).
    fn pump(&mut self, now_ms: u64) -> Result<(), String>;
}

fn fail(error: MsgError) -> String {
    format!("messenger: {}", error.message())
}

/// Publish `system/net/<ifname>/link`, connecting to the broker on first use.
fn publish_link(bus: &mut Option<central::Bus>, ifname: &str, up: bool, changes: u32) {
    if bus.is_none() {
        *bus = central::Bus::connect().ok();
    }
    if let Some(bus) = bus.as_mut() {
        let event = nic_wire::LinkEvent { up, changes };
        let _ = nic_wire::publish_system_net_link(bus, ifname, &event);
    }
}

/// Serve `radio` as `ifname` until something fatal happens. `own_uid` is the
/// program's uid (a probe it spawns may attach, see [`hw::may_attach`]).
pub fn run<R: Hardware>(radio: R, ifname: &str, own_uid: u32) -> Result<(), String> {
    let (hw_published, hw_server) = messenger::create_pair().map_err(fail)?;
    let (nic_published, nic_server) = messenger::create_pair().map_err(fail)?;
    let mut nic = NicService::new(SoftMac::new(radio), ifname);
    let mut hw = HwService::new(ifname, own_uid);
    let hw_name = hw::service_name(ifname);
    let nic_name = nic_api::service_name(ifname);
    registry::register(&hw_name, &hw_published, &[hw::INTERFACE], 0).map_err(fail)?;
    registry::register(&nic_name, &nic_published, &[nic_api::INTERFACE], 0).map_err(fail)?;
    let facts = nic.mac().info();
    sys::write_str(&format!(
        "WIFI:READY if={ifname} hw={hw_name} nic={nic_name} mtu={} max_mgmt={}\n",
        facts.mtu, facts.max_mgmt
    ));
    let mut bus = None;
    publish_link(&mut bus, ifname, false, 0);
    let mut buffer = vec![0u8; messenger::DEFAULT_BUFFER];
    loop {
        let now = sys::monotonic_ms();
        let wake = nic.mac().next_deadline().unwrap_or(now + IDLE_MS).min(now + IDLE_MS);
        let deadline = lazyos_sys::time::deadline_after_ms(wake.saturating_sub(now));
        let extra = nic.mac().radio().wake_endpoint();
        let _ = match extra {
            Some(extra) => wait::wait_any(&[hw_server, nic_server, extra], 0, Some(deadline)),
            None => wait::wait_any(&[hw_server, nic_server], 0, Some(deadline)),
        };
        let now = sys::monotonic_ms();
        nic.mac().radio_mut().pump(now)?;
        nic.mac().poll(now);
        for _ in 0..REQUEST_BUDGET {
            let Some(message) = hw_server.poll_recv_with(&mut buffer).map_err(fail)? else {
                break;
            };
            let reply = hw.dispatch(nic.mac(), &message, now);
            if let Some(txn) = message.txn {
                hw_server.reply_or_drop(txn, &reply).map_err(fail)?;
            }
        }
        for _ in 0..REQUEST_BUDGET {
            let Some(message) = nic_server.poll_recv_with(&mut buffer).map_err(fail)? else {
                break;
            };
            let reply = nic.dispatch(&message).unwrap_or_else(|error| {
                messenger::services::error_reply(message.interface_id(), message.method(), error)
            });
            if let Some(txn) = message.txn {
                nic_server.reply_or_drop(txn, &reply).map_err(fail)?;
            }
        }
        // Requests may have queued frames for the air: send them now.
        nic.mac().radio_mut().pump(now)?;
        hw.flush(nic.mac());
        hw.watch_owner(nic.mac(), now);
        let changed = nic
            .pump(now)
            .map_err(|fatal| format!("engine: {fatal:?}"))?;
        if changed {
            let stats = nic.engine.stats();
            let up = nic.engine.link();
            sys::write_str(&format!("WIFI:LINK if={ifname} up={up}\n"));
            publish_link(&mut bus, ifname, up, stats.link_changes);
        }
        nic.mac().radio_mut().pump(now)?;
    }
}
