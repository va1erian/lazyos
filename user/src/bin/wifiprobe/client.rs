//! A blocking client of `os.lazy.net.wifi.hw.v1`: the calls `wifiprobe`
//! makes, and its event channel (the far end handed over in `Attach`).

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use libmessenger::Parcel;
use user::messenger::services::init::error_field;
use user::messenger::{self, registry, Endpoint, Error};
use user::sys;
use user::wifi::hw::{self, wire};

/// One event from the radio.
pub enum Event {
    ScanDone { scan_id: u32, aborted: bool },
    RxMgmt { kind: u32, frame: Vec<u8> },
    BeaconLoss,
    Deauthenticated { reason: u32 },
}

pub struct HwClient {
    endpoint: Endpoint,
    events: Endpoint,
    buf: Vec<u8>,
}

fn text(error: Error) -> String {
    String::from(error.message())
}

impl HwClient {
    /// Resolve the radio `ifname`, retrying for up to `wait_ms`.
    pub fn connect(ifname: &str, wait_ms: u64) -> Result<HwClient, String> {
        let name = hw::service_name(ifname);
        let give_up = sys::monotonic_ms() + wait_ms;
        let endpoint = loop {
            match registry::resolve(&name) {
                Ok(endpoint) => break endpoint,
                Err(error) if sys::monotonic_ms() >= give_up => {
                    return Err(format!("resolve {name}: {}", error.message()));
                }
                Err(_) => {
                    let _ = sys::sleep_ns(200_000_000);
                }
            }
        };
        let (mine, theirs) = messenger::create_pair().map_err(text)?;
        let client = HwClient {
            endpoint,
            events: mine,
            buf: vec![0; messenger::DEFAULT_BUFFER],
        };
        let (body, objects) = wire::encode_attach_args(&wire::AttachArgs {
            events: theirs.handle(),
        })
        .map_err(|e| format!("encode: {e:?}"))?;
        let mut request = hw::parcel(wire::METHOD_ATTACH, body, false);
        request.objects = objects;
        client.call(request).map_err(|e| format!("Attach: {e}"))?;
        Ok(client)
    }

    fn call(&self, request: Parcel) -> Result<Parcel, String> {
        let reply = self.endpoint.call(&request, None).map_err(text)?;
        match error_field(&reply).map_err(text)? {
            Some(code) => Err(format!("errno {code}")),
            None => Ok(reply),
        }
    }

    fn simple(&self, method: u32, body: Vec<u8>) -> Result<Parcel, String> {
        self.call(hw::parcel(method, body, false))
    }

    pub fn info(&self) -> Result<wire::HwInfo, String> {
        let reply = self.simple(wire::METHOD_INFO, Vec::new())?;
        Ok(wire::decode_info_reply(&reply.body)
            .map_err(|e| format!("{e:?}"))?
            .info)
    }

    pub fn set_country(&self, alpha2: &str) -> Result<(), String> {
        let body = wire::encode_set_country_args(&wire::SetCountryArgs {
            alpha2: String::from(alpha2),
        })
        .map_err(|e| format!("{e:?}"))?;
        self.simple(wire::METHOD_SETCOUNTRY, body).map(|_| ())
    }

    pub fn scan(&self, request: wire::ScanRequest) -> Result<u32, String> {
        let body = wire::encode_scan_args(&wire::ScanArgs { request }).map_err(|e| format!("{e:?}"))?;
        let reply = self.simple(wire::METHOD_SCAN, body)?;
        Ok(wire::decode_scan_reply(&reply.body)
            .map_err(|e| format!("{e:?}"))?
            .scan_id)
    }

    /// Every result of `scan_id`, page by page.
    pub fn scan_results(&self, scan_id: u32) -> Result<Vec<wire::ScanEntry>, String> {
        let mut all = Vec::new();
        loop {
            let body = wire::encode_scan_results_args(&wire::ScanResultsArgs {
                scan_id,
                first: all.len() as u32,
                max: 64,
            })
            .map_err(|e| format!("{e:?}"))?;
            let reply = self.simple(wire::METHOD_SCANRESULTS, body)?;
            let page = wire::decode_scan_results_reply(&reply.body).map_err(|e| format!("{e:?}"))?;
            let more = page.more && !page.results.is_empty();
            all.extend(page.results);
            if !more {
                return Ok(all);
            }
        }
    }

    pub fn join(&self, bssid: &[u8; 6], channel: u32, band: u32) -> Result<(), String> {
        let request = wire::JoinRequest {
            bssid: bssid.to_vec(),
            channel,
            band,
            width: wire::WIDTH_MHZ20,
        };
        let body = wire::encode_join_args(&wire::JoinArgs { request }).map_err(|e| format!("{e:?}"))?;
        self.simple(wire::METHOD_JOIN, body).map(|_| ())
    }

    pub fn tx(&self, kind: u32, frame: Vec<u8>) -> Result<(), String> {
        let body = wire::encode_tx_mgmt_args(&wire::TxMgmtArgs { kind, frame })
            .map_err(|e| format!("{e:?}"))?;
        self.simple(wire::METHOD_TXMGMT, body).map(|_| ())
    }

    pub fn set_state(&self, state: u32, aid: u32) -> Result<(), String> {
        let body = wire::encode_set_state_args(&wire::SetStateArgs { state, aid })
            .map_err(|e| format!("{e:?}"))?;
        self.simple(wire::METHOD_SETSTATE, body).map(|_| ())
    }

    pub fn leave(&self, reason: u32) -> Result<(), String> {
        let body = wire::encode_leave_args(&wire::LeaveArgs { reason }).map_err(|e| format!("{e:?}"))?;
        self.simple(wire::METHOD_LEAVE, body).map(|_| ())
    }

    pub fn set_key(&self, kind: u32, index: u32, key: &[u8], rsc: &[u8], addr: &[u8; 6]) -> Result<(), String> {
        let args = wire::SetKeyArgs {
            kind,
            index,
            cipher: wire::CIPHER_CCMP128,
            key: key.to_vec(),
            rsc: rsc.to_vec(),
            addr: addr.to_vec(),
        };
        let body = wire::encode_set_key_args(&args).map_err(|e| format!("{e:?}"))?;
        self.simple(wire::METHOD_SETKEY, body).map(|_| ())
    }

    /// The next event before `deadline_ms` (monotonic), `None` on timeout.
    pub fn next_event(&mut self, deadline_ms: u64) -> Result<Option<Event>, String> {
        let now = sys::monotonic_ms();
        if now >= deadline_ms {
            return Ok(None);
        }
        let deadline = lazyos_sys::time::deadline_after_ms(deadline_ms - now);
        let message = match self.events.recv_with(&mut self.buf, Some(deadline)) {
            Ok(message) => message,
            Err(error) if error.errno() == Some(-user::messenger::errno::ETIMEDOUT) => return Ok(None),
            Err(error) => return Err(String::from(error.message())),
        };
        let body = &message.parcel.body;
        let bad = |e| format!("event: {e:?}");
        Ok(Some(match message.method() {
            wire::METHOD_SCANDONE => {
                let a = wire::decode_scan_done_args(body).map_err(bad)?;
                Event::ScanDone {
                    scan_id: a.scan_id,
                    aborted: a.aborted,
                }
            }
            wire::METHOD_RXMGMT => {
                let a = wire::decode_rx_mgmt_args(body).map_err(bad)?;
                Event::RxMgmt {
                    kind: a.kind,
                    frame: a.frame,
                }
            }
            wire::METHOD_BEACONLOSS => Event::BeaconLoss,
            wire::METHOD_DEAUTHENTICATED => Event::Deauthenticated {
                reason: wire::decode_deauthenticated_args(body).map_err(bad)?.reason,
            },
            other => return Err(format!("unknown event method {other}")),
        }))
    }
}
