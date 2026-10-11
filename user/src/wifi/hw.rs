//! `os.lazy.net.wifi.hw.v1` (`idl/wifi.midl`): request dispatch onto a
//! [`SoftMac`], and the events sent back on the owner's channel.
//!
//! The owner is the kernel-stamped sender of `Attach`; nothing in a request
//! names a caller. Who may attach at all is [`may_attach`]: the station
//! manager (`netpolicy::may_attach_wifi_hw`: `_wlan`, unlabelled, outside
//! any login session) or, the same way unlabelled and session-less, the
//! radio program's own uid (the `wifiprobe` test client `wifisim` spawns).
//! Root is not named. An application or a session user may read `Info` and
//! `Stats` but never owns the radio, so a crashed `wlanmd` cannot be
//! replaced by anything that could inject management frames.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use libmessenger::{flags, Header, Parcel, VERSION};
use wifimac::{Error as MacError, Event, JoinRequest, KeyRequest, Radio, ScanRequest, SoftMac};

use crate::messenger::{errno, services, Endpoint, Error as MsgError, Message};
use crate::sys::Cred;

/// The generated `os.lazy.net.wifi.hw.v1` stubs.
pub use messenger_generated::os_lazy_net_wifi_hw_v1 as wire;

pub const INTERFACE: u64 = wire::INTERFACE_ID;
/// The registry namespace of radios: `os.lazy.net.wifi.hw/<ifname>`.
pub const NAME_PREFIX: &str = "os.lazy.net.wifi.hw";

/// Octets of frames one `ScanResults` page carries: a reply must fit the
/// client's 16 KiB receive buffer with its encoding.
const PAGE_BYTES: usize = 12 * 1024;
/// How often the owner's event channel is checked for a vanished owner.
const PROBE_MS: u64 = 1000;

/// The registered name of the radio of `ifname`.
pub fn service_name(ifname: &str) -> String {
    format!("{NAME_PREFIX}/{ifname}")
}

/// Whether `caller` may become the radio's owner (see the module notes).
pub fn may_attach(caller: &Cred, own_uid: u32) -> bool {
    netpolicy::may_attach_wifi_hw(caller.uid, caller.label_id, caller.session)
        || (caller.uid == own_uid && caller.label_id == 0 && caller.session == 0)
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
        MacError::Busy => "the radio is busy (attached, or scanning)",
        MacError::Denied => "only the radio's owner may do this",
        MacError::Again => "no regulatory domain yet (SetCountry first)",
        MacError::Invalid => "invalid request for this radio or domain",
        MacError::NotFound => "no such scan",
        MacError::NotConnected => "not joined to a BSS",
        MacError::NoSpace => "no free key slot",
        MacError::Radio => "the radio failed",
    }
}

type Refusal = (i64, &'static str);

fn mac_err(error: MacError) -> Refusal {
    (error.errno(), message_of(error))
}

fn parcel_err(_: libmessenger::Error) -> Refusal {
    (errno::EINVAL, "malformed request")
}

/// The radio's side of `hw.v1`: who owns it and the channel its events go on.
pub struct HwService {
    ifname: String,
    own_uid: u32,
    events: Option<Endpoint>,
    next_probe_ms: u64,
    probe_buf: Vec<u8>,
}

impl HwService {
    pub fn new(ifname: &str, own_uid: u32) -> HwService {
        HwService {
            ifname: String::from(ifname),
            own_uid,
            events: None,
            next_probe_ms: 0,
            probe_buf: alloc::vec![0; 256],
        }
    }

    /// Answer one request: the reply, or the structured error reply.
    pub fn dispatch<R: Radio>(
        &mut self,
        mac: &mut SoftMac<R>,
        message: &Message,
        now: u64,
    ) -> Parcel {
        let method = message.method();
        if message.interface_id() != INTERFACE {
            return services::refusal(
                message.interface_id(),
                method,
                errno::EINVAL,
                "wrong interface",
            );
        }
        match self.route(mac, message, now) {
            Ok(body) => parcel(method, body, false),
            Err((code, text)) => services::refusal(INTERFACE, method, code, text),
        }
    }

    fn route<R: Radio>(
        &mut self,
        mac: &mut SoftMac<R>,
        message: &Message,
        now: u64,
    ) -> Result<Vec<u8>, Refusal> {
        let body = &message.parcel.body;
        let sender = message.sender;
        let empty = Ok(Vec::new());
        match message.method() {
            wire::METHOD_INFO => self.info(mac),
            wire::METHOD_STATS => stats(mac),
            wire::METHOD_ATTACH => self.attach(mac, message),
            wire::METHOD_DETACH => {
                mac.detach(sender).map_err(mac_err)?;
                self.drop_events();
                empty
            }
            wire::METHOD_SETCOUNTRY => {
                let args = wire::decode_set_country_args(body).map_err(parcel_err)?;
                mac.set_country(sender, &args.alpha2).map_err(mac_err)?;
                empty
            }
            wire::METHOD_SCAN => {
                let r = wire::decode_scan_args(body).map_err(parcel_err)?.request;
                let request = ScanRequest {
                    channels: r.channels,
                    band: r.band,
                    ssids: r.ssids,
                    active: r.active,
                    dwell_ms: r.dwell_ms,
                };
                let scan_id = mac.scan(sender, &request, now).map_err(mac_err)?;
                wire::encode_scan_reply(&wire::ScanReply { scan_id }).map_err(parcel_err)
            }
            wire::METHOD_ABORTSCAN => {
                let args = wire::decode_abort_scan_args(body).map_err(parcel_err)?;
                mac.abort_scan(sender, args.scan_id, now).map_err(mac_err)?;
                empty
            }
            wire::METHOD_SCANRESULTS => {
                let a = wire::decode_scan_results_args(body).map_err(parcel_err)?;
                let page = mac
                    .scan_results(sender, a.scan_id, a.first, a.max, PAGE_BYTES, now)
                    .map_err(mac_err)?;
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
                let reply = wire::ScanResultsReply {
                    results,
                    more: page.more,
                };
                wire::encode_scan_results_reply(&reply).map_err(parcel_err)
            }
            wire::METHOD_JOIN => {
                let r = wire::decode_join_args(body).map_err(parcel_err)?.request;
                let request = JoinRequest {
                    bssid: r.bssid,
                    channel: r.channel,
                    band: r.band,
                    width: r.width,
                };
                mac.join(sender, &request, now).map_err(mac_err)?;
                empty
            }
            wire::METHOD_TXMGMT => {
                let args = wire::decode_tx_mgmt_args(body).map_err(parcel_err)?;
                mac.tx_mgmt(sender, args.kind, &args.frame)
                    .map_err(mac_err)?;
                empty
            }
            wire::METHOD_SETSTATE => {
                let args = wire::decode_set_state_args(body).map_err(parcel_err)?;
                mac.set_state(sender, args.state, args.aid)
                    .map_err(mac_err)?;
                empty
            }
            wire::METHOD_SETKEY => {
                let mut args = wire::decode_set_key_args(body).map_err(parcel_err)?;
                let request = KeyRequest {
                    kind: args.kind,
                    index: args.index,
                    cipher: args.cipher,
                    key: &args.key,
                    rsc: &args.rsc,
                    addr: &args.addr,
                };
                let result = mac.set_key(sender, &request);
                // The temporal key lives in the radio's slot only.
                args.key.fill(0);
                result.map_err(mac_err)?;
                empty
            }
            wire::METHOD_DELKEY => {
                let args = wire::decode_del_key_args(body).map_err(parcel_err)?;
                mac.del_key(sender, args.kind, args.index)
                    .map_err(mac_err)?;
                empty
            }
            wire::METHOD_LEAVE => {
                let args = wire::decode_leave_args(body).map_err(parcel_err)?;
                mac.leave(sender, args.reason).map_err(mac_err)?;
                empty
            }
            _ => Err((errno::EINVAL, "unknown method")),
        }
    }

    fn info<R: Radio>(&self, mac: &SoftMac<R>) -> Result<Vec<u8>, Refusal> {
        let facts = mac.info();
        let info = wire::HwInfo {
            mac: facts.mac.to_vec(),
            interface: self.ifname.clone(),
            bands: facts.bands,
            ciphers: facts.ciphers,
            scan_offload: facts.scan_offload,
            max_mgmt: facts.max_mgmt,
            max_scan_ssids: facts.max_scan_ssids,
            max_scan_channels: facts.max_scan_channels,
            key_slots: facts.key_slots,
            features: facts.features,
        };
        wire::encode_info_reply(&wire::InfoReply { info }).map_err(parcel_err)
    }

    /// `Attach`: the decoded request owns the event channel; every way out
    /// but success gives it back.
    fn attach<R: Radio>(
        &mut self,
        mac: &mut SoftMac<R>,
        message: &Message,
    ) -> Result<Vec<u8>, Refusal> {
        let args = message
            .decode(wire::decode_attach_args)
            .map_err(|_| (errno::EINVAL, "malformed request"))?;
        let events = Endpoint::from_raw(args.events);
        let outcome = if may_attach(&message.caller(), self.own_uid) {
            mac.attach(message.sender).map_err(mac_err)
        } else {
            Err((errno::EPERM, "only the station manager may own the radio"))
        };
        match outcome {
            Ok(()) => {
                self.drop_events();
                self.events = Some(events);
                crate::sys::write_str(&format!(
                    "WIFI:HW:ATTACH if={} owner={} uid={}\n",
                    self.ifname,
                    message.sender,
                    message.caller().uid
                ));
                Ok(Vec::new())
            }
            Err(refusal) => {
                let _ = events.release();
                Err(refusal)
            }
        }
    }

    fn drop_events(&mut self) {
        if let Some(events) = self.events.take() {
            let _ = events.release();
        }
    }

    /// The owner is gone: the radio goes idle and its channel is let go.
    fn owner_gone<R: Radio>(&mut self, mac: &mut SoftMac<R>, why: &str) {
        crate::sys::write_str(&format!("WIFI:HW:DETACH if={} {why}\n", self.ifname));
        mac.release();
        self.drop_events();
    }

    /// Send every queued event to the owner. A dead channel releases the
    /// radio; a full inbox loses the event (the owner's timers recover).
    pub fn flush<R: Radio>(&mut self, mac: &mut SoftMac<R>) {
        if mac.owner().is_none() {
            self.drop_events();
            while mac.next_event().is_some() {}
            return;
        }
        while let Some(event) = mac.next_event() {
            let Some(events) = self.events else { return };
            let Ok(parcel) = event_parcel(&event) else {
                continue;
            };
            if let Err(error) = events.send(&parcel) {
                if gone(&error) {
                    return self.owner_gone(mac, "owner gone (send failed)");
                }
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
        if let Err(error) = events.poll_recv_with(&mut self.probe_buf) {
            if gone(&error) {
                self.owner_gone(mac, "owner gone");
            }
        }
    }
}

fn gone(error: &MsgError) -> bool {
    matches!(error.errno(), Some(code) if code == -errno::EPIPE || code == -9)
}

fn stats<R: Radio>(mac: &SoftMac<R>) -> Result<Vec<u8>, Refusal> {
    let s = mac.stats();
    let stats = wire::HwStats {
        scans: s.scans,
        mgmt_rx: s.mgmt_rx,
        mgmt_tx: s.mgmt_tx,
        mgmt_dropped: s.mgmt_dropped,
        beacon_losses: s.beacon_losses,
        deauths: s.deauths,
        fw_errors: s.fw_errors,
        resets: s.resets,
    };
    wire::encode_stats_reply(&wire::StatsReply { stats }).map_err(parcel_err)
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
