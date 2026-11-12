//! `os.lazy.net.wifi.hw.v1` (`idl/wifi.midl`): request dispatch onto a
//! [`SoftMac`], and the events sent back on the owner's channel.
//!
//! The owner is the kernel-stamped sender of `Attach`, remembered whole: its
//! task slot and its credentials (uid, label, session), all compared on
//! every owner-only call ([`wifimac::Caller`]), so a slot another process
//! reuses before the owner's exit is noticed owns nothing. Nothing in a
//! request names a caller. Who may attach at all is [`may_attach`]: the
//! station manager (`netpolicy::may_attach_wifi_hw`: `_wlan`, unlabelled,
//! outside any login session) and, in the simulator alone, `_wifisim` itself
//! the same way unlabelled and session-less (the `wifiprobe` test client
//! `wifisim` spawns). Root is not named. An application or a session user
//! may read `Info` and `Stats` but never owns the radio, so a crashed
//! `wlanmd` cannot be replaced by anything that could inject management
//! frames.

use crate::trace;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use libmessenger::server::{Caller as StampedCaller, Fault};
use libmessenger::{flags, Header, Parcel, VERSION};
use wifimac::{
    Caller, Error as MacError, Event, JoinRequest, KeyRequest, Radio, ScanRequest, SoftMac,
};

use crate::messenger::{errno, Endpoint, Error as MsgError, Message};
use crate::sys::Cred;

/// The generated `os.lazy.net.wifi.hw.v1` stubs.
pub use messenger_generated::os_lazy_net_wifi_hw_v1 as wire;

pub const INTERFACE: u64 = wire::INTERFACE_ID;

/// Octets of frames one `ScanResults` page carries: a reply must fit the
/// client's 16 KiB receive buffer with its encoding.
const PAGE_BYTES: usize = 12 * 1024;
/// How often the owner's event channel is checked for a vanished owner.
const PROBE_MS: u64 = 1000;
/// Messages the owner sent on its event channel (nothing is expected there)
/// drained per check, so a flood cannot hold the loop.
const PROBE_DRAIN: usize = 64;

/// The registered name of the radio of `ifname`.
pub fn service_name(ifname: &str) -> String {
    format!("{}{ifname}", wire::SERVICE_INSTANCE_PREFIX)
}

/// Whether `caller` may become the radio's owner (see the module notes).
/// `probe_uid` is the simulator's own uid when it runs its test client, and
/// `None` for a real radio.
pub fn may_attach(caller: &Cred, probe_uid: Option<u32>) -> bool {
    let system = caller.label_id == 0 && caller.session == 0;
    netpolicy::may_attach_wifi_hw(caller.uid, caller.label_id, caller.session)
        || (system && probe_uid == Some(caller.uid))
}

/// The probe identity a radio program running as `own_uid` accepts: only
/// the simulator (`_wifisim`) spawns a test client under its own uid.
pub fn probe_uid(own_uid: u32) -> Option<u32> {
    (own_uid == netpolicy::WIFISIM_UID).then_some(own_uid)
}

/// Whether a request of `method` carried secret material (a temporal key),
/// which the caller wipes from its receive buffer once it was handled.
pub fn carries_secret(method: u32) -> bool {
    method == wire::METHOD_SETKEY
}

/// A parcel of this interface; `oneway` for an event.
pub fn parcel(method: u32, body: Vec<u8>, oneway: bool) -> Parcel {
    Parcel {
        header: Header {
            version: VERSION,
            flags: if oneway { flags::ONE_WAY } else { 0 },
            interface_id: INTERFACE,
            method,
            txn_id: 0,
            reply_to: 0,
            deadline_ns: 0,
        },
        body,
        objects: Vec::new(),
    }
}

fn message_of(error: MacError) -> &'static str {
    match error {
        MacError::Busy => "the radio is busy (attached, scanning, or its transmit queue is full)",
        MacError::Denied => "only the radio's owner may do this",
        MacError::Again => "no regulatory domain yet (SetCountry first)",
        MacError::Invalid => "invalid request for this radio or domain",
        MacError::NotFound => "no such scan",
        MacError::NotConnected => "not joined to a BSS",
        MacError::NoSpace => "no free key slot, or the key's packet numbers are spent",
        MacError::Radio => "the radio failed",
    }
}

type Refusal = (i64, &'static str);

fn mac_err(error: MacError) -> Refusal {
    (error.errno(), message_of(error))
}

/// The radio's side of `hw.v1`: who owns it and the channel its events go on.
pub struct HwService {
    ifname: String,
    probe_uid: Option<u32>,
    events: Option<Endpoint>,
    next_probe_ms: u64,
    probe_buf: Vec<u8>,
}

impl HwService {
    pub fn new(ifname: &str, own_uid: u32) -> HwService {
        HwService {
            ifname: String::from(ifname),
            probe_uid: probe_uid(own_uid),
            events: None,
            next_probe_ms: 0,
            probe_buf: alloc::vec![0; 256],
        }
    }

    /// Answer one request through the generated server. A `SetKey`
    /// request's body is wiped once handled.
    pub fn dispatch<R: Radio>(
        &mut self,
        mac: &mut SoftMac<R>,
        message: &mut Message,
        now: u64,
    ) -> Parcel {
        let reply = wire::dispatch(
            &mut HwHandler {
                svc: self,
                mac,
                now,
            },
            message,
        );
        if carries_secret(message.method()) {
            message.parcel.body.fill(0);
        }
        reply
    }

    fn drop_events(&mut self) {
        if let Some(events) = self.events.take() {
            let _ = events.release();
        }
    }

    /// The owner is gone: the radio goes idle and its channel is let go.
    fn owner_gone<R: Radio>(&mut self, mac: &mut SoftMac<R>, why: &str) {
        crate::trace::line!(trace::Channel::Wifi, "HW:DETACH if={} {why}", self.ifname);
        mac.release();
        self.drop_events();
    }

    /// Send the queued events to the owner. A dead channel releases the
    /// radio; a full inbox puts the event back first in the soft-MAC's
    /// queue, which keeps the control events and drops frames first while
    /// the owner catches up.
    pub fn flush<R: Radio>(&mut self, mac: &mut SoftMac<R>) {
        if mac.owner().is_none() {
            self.drop_events();
            while mac.next_event().is_some() {}
            return;
        }
        let Some(events) = self.events else { return };
        while let Some(event) = mac.next_event() {
            let Ok(parcel) = event_parcel(&event) else {
                continue;
            };
            match events.send(&parcel) {
                Ok(()) => {}
                Err(error) if gone(&error) => {
                    return self.owner_gone(mac, "owner gone (send failed)");
                }
                Err(error) if error.errno() == Some(-errno::EAGAIN) => {
                    mac.requeue_event(event);
                    return;
                }
                // Anything else is this event's own problem: it is lost.
                Err(_) => {}
            }
        }
    }

    /// Every second, look whether the owner's end of the channel still
    /// exists.
    pub fn watch_owner<R: Radio>(&mut self, mac: &mut SoftMac<R>, now: u64) {
        if now < self.next_probe_ms {
            return;
        }
        self.next_probe_ms = now + PROBE_MS;
        let Some(events) = self.events else { return };
        for _ in 0..PROBE_DRAIN {
            match events.poll_recv_with(&mut self.probe_buf) {
                Ok(Some(_)) => {}
                Ok(None) => return,
                Err(error) if gone(&error) => return self.owner_gone(mac, "owner gone"),
                // Too big for the probe buffer, or malformed: dequeued anyway.
                Err(_) => {}
            }
        }
    }
}

/// The channel's other end is gone, or the handle no longer names it.
fn gone(error: &MsgError) -> bool {
    let dead = [errno::EPIPE, errno::EBADF, errno::ENOENT];
    matches!(error.errno(), Some(code) if dead.contains(&-code))
}

/// The radio behind one request (the generated server): the service state,
/// the soft-MAC and the time the request was taken.
struct HwHandler<'a, R: Radio> {
    svc: &'a mut HwService,
    mac: &'a mut SoftMac<R>,
    now: u64,
}

fn fault((code, text): Refusal) -> Fault {
    Fault::new(code as u32, text)
}

/// The wifimac identity of the kernel-stamped sender.
fn sender_of(cx: &StampedCaller) -> Caller {
    Caller {
        task: cx.sender,
        uid: cx.uid,
        label_id: cx.label_id,
        session: cx.session,
    }
}

impl<R: Radio> wire::Server for HwHandler<'_, R> {
    fn info(&mut self, _: &StampedCaller) -> Result<wire::HwInfo, Fault> {
        let facts = self.mac.info();
        Ok(wire::HwInfo {
            mac: facts.mac.to_vec(),
            interface: self.svc.ifname.clone(),
            bands: facts.bands,
            ciphers: facts.ciphers,
            scan_offload: facts.scan_offload,
            max_mgmt: facts.max_mgmt,
            max_scan_ssids: facts.max_scan_ssids,
            max_scan_channels: facts.max_scan_channels,
            key_slots: facts.key_slots,
            features: facts.features,
        })
    }

    fn stats(&mut self, _: &StampedCaller) -> Result<wire::HwStats, Fault> {
        let s = self.mac.stats();
        Ok(wire::HwStats {
            scans: s.scans,
            mgmt_rx: s.mgmt_rx,
            mgmt_tx: s.mgmt_tx,
            mgmt_dropped: s.mgmt_dropped,
            beacon_losses: s.beacon_losses,
            deauths: s.deauths,
            fw_errors: s.fw_errors,
            resets: s.resets,
        })
    }

    /// `Attach`: the decoded request owns the event channel; every way out
    /// but success gives it back.
    fn attach(&mut self, cx: &StampedCaller, events: u64) -> Result<(), Fault> {
        let events = Endpoint::from_raw(events);
        let cred = Cred::new(cx.uid, cx.gid, cx.caps, cx.label_id, cx.session);
        let outcome = if may_attach(&cred, self.svc.probe_uid) {
            self.mac.attach(sender_of(cx)).map_err(mac_err)
        } else {
            Err((errno::EPERM, "only the station manager may own the radio"))
        };
        match outcome {
            Ok(()) => {
                self.svc.drop_events();
                self.svc.events = Some(events);
                crate::trace::line!(
                    trace::Channel::Wifi,
                    "HW:ATTACH if={} owner={} uid={}",
                    self.svc.ifname,
                    cx.sender,
                    cx.uid
                );
                Ok(())
            }
            Err(refusal) => {
                let _ = events.release();
                Err(fault(refusal))
            }
        }
    }

    fn detach(&mut self, cx: &StampedCaller) -> Result<(), Fault> {
        self.mac
            .detach(sender_of(cx))
            .map_err(mac_err)
            .map_err(fault)?;
        self.svc.drop_events();
        Ok(())
    }

    fn set_country(&mut self, cx: &StampedCaller, alpha2: String) -> Result<(), Fault> {
        self.mac
            .set_country(sender_of(cx), &alpha2)
            .map_err(|e| fault(mac_err(e)))
    }

    fn scan(&mut self, cx: &StampedCaller, r: wire::ScanRequest) -> Result<u32, Fault> {
        let request = ScanRequest {
            channels: r.channels,
            band: r.band,
            ssids: r.ssids,
            active: r.active,
            dwell_ms: r.dwell_ms,
        };
        self.mac
            .scan(sender_of(cx), &request, self.now)
            .map_err(|e| fault(mac_err(e)))
    }

    fn abort_scan(&mut self, cx: &StampedCaller, scan_id: u32) -> Result<(), Fault> {
        self.mac
            .abort_scan(sender_of(cx), scan_id, self.now)
            .map_err(|e| fault(mac_err(e)))
    }

    fn scan_results(
        &mut self,
        cx: &StampedCaller,
        scan_id: u32,
        first: u32,
        max: u32,
    ) -> Result<wire::ScanResultsReply, Fault> {
        let page = self
            .mac
            .scan_results(sender_of(cx), scan_id, first, max, PAGE_BYTES, self.now)
            .map_err(|e| fault(mac_err(e)))?;
        let results = page
            .entries
            .iter()
            .map(|(entry, age_ms)| wire::ScanEntry {
                frame: entry.frame.clone(),
                channel: u32::from(entry.channel),
                band: entry.band as u32,
                rssi_dbm: i32::from(entry.rssi_dbm),
                age_ms: *age_ms,
            })
            .collect();
        Ok(wire::ScanResultsReply {
            results,
            more: page.more,
        })
    }

    fn join(&mut self, cx: &StampedCaller, r: wire::JoinRequest) -> Result<(), Fault> {
        let request = JoinRequest {
            bssid: r.bssid,
            channel: r.channel,
            band: r.band,
            width: r.width,
        };
        self.mac
            .join(sender_of(cx), &request, self.now)
            .map_err(|e| fault(mac_err(e)))
    }

    fn tx_mgmt(&mut self, cx: &StampedCaller, kind: u32, frame: Vec<u8>) -> Result<(), Fault> {
        self.mac
            .tx_mgmt(sender_of(cx), kind, &frame)
            .map_err(|e| fault(mac_err(e)))
    }

    fn set_state(&mut self, cx: &StampedCaller, state: u32, aid: u32) -> Result<(), Fault> {
        self.mac
            .set_state(sender_of(cx), state, aid)
            .map_err(|e| fault(mac_err(e)))
    }

    fn set_key(
        &mut self,
        cx: &StampedCaller,
        kind: u32,
        index: u32,
        cipher: u32,
        mut key: Vec<u8>,
        rsc: Vec<u8>,
        addr: Vec<u8>,
    ) -> Result<(), Fault> {
        let request = KeyRequest {
            kind,
            index,
            cipher,
            key: &key,
            rsc: &rsc,
            addr: &addr,
        };
        let result = self.mac.set_key(sender_of(cx), &request);
        // The temporal key lives in the radio's slot only.
        key.fill(0);
        result.map_err(|e| fault(mac_err(e)))
    }

    fn del_key(&mut self, cx: &StampedCaller, kind: u32, index: u32) -> Result<(), Fault> {
        self.mac
            .del_key(sender_of(cx), kind, index)
            .map_err(|e| fault(mac_err(e)))
    }

    fn leave(&mut self, cx: &StampedCaller, reason: u32) -> Result<(), Fault> {
        self.mac
            .leave(sender_of(cx), reason)
            .map_err(|e| fault(mac_err(e)))
    }

    // The events travel driver to client; a client that sends one to the
    // driver is ignored.
    fn scan_done(&mut self, _: &StampedCaller, _: u32, _: bool) {}
    fn rx_mgmt(&mut self, _: &StampedCaller, _: u32, _: i32, _: Vec<u8>) {}
    fn beacon_loss(&mut self, _: &StampedCaller) {}
    fn deauthenticated(&mut self, _: &StampedCaller, _: u32) {}
}

/// The one-way parcel of an event.
pub fn event_parcel(event: &Event) -> Result<Parcel, libmessenger::Error> {
    let (method, body) = match event {
        Event::ScanDone { scan_id, aborted } => (
            wire::METHOD_SCANDONE,
            wire::encode_scan_done_args(&wire::ScanDoneArgs {
                scan_id: *scan_id,
                aborted: *aborted,
            })?,
        ),
        Event::RxMgmt {
            kind,
            rssi_dbm,
            frame,
        } => (
            wire::METHOD_RXMGMT,
            wire::encode_rx_mgmt_args(&wire::RxMgmtArgs {
                kind: *kind as u32,
                rssi_dbm: *rssi_dbm,
                frame: frame.clone(),
            })?,
        ),
        Event::BeaconLoss => (wire::METHOD_BEACONLOSS, Vec::new()),
        Event::Deauthenticated { reason } => (
            wire::METHOD_DEAUTHENTICATED,
            wire::encode_deauthenticated_args(&wire::DeauthenticatedArgs {
                reason: u32::from(*reason),
            })?,
        ),
    };
    Ok(parcel(method, body, true))
}
