//! A blocking client of `os.lazy.net.wifi.hw.v1`: the calls `wifiprobe`
//! makes, and its event channel (the far end handed over in `Attach`).

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use libmessenger::client::CallError;
use user::messenger::wifi::{self, Radio};
use user::messenger::{self, Endpoint, Error};
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
    radio: Radio,
    events: Endpoint,
    buf: Vec<u8>,
}

fn text(error: Error) -> String {
    String::from(error.message())
}

fn call_text(error: CallError) -> String {
    String::from(error.message())
}

impl HwClient {
    /// Resolve the radio `ifname`, retrying for up to `wait_ms`.
    pub fn connect(ifname: &str, wait_ms: u64) -> Result<HwClient, String> {
        let give_up = sys::monotonic_ms() + wait_ms;
        let radio = loop {
            match wifi::connect_radio(ifname) {
                Ok(radio) => break radio,
                Err(error) if sys::monotonic_ms() >= give_up => {
                    return Err(format!(
                        "resolve {}: {}",
                        hw::service_name(ifname),
                        error.message()
                    ));
                }
                Err(_) => {
                    let _ = sys::sleep_ns(200_000_000);
                }
            }
        };
        let (mine, theirs) = messenger::create_pair().map_err(text)?;
        let client = HwClient {
            radio,
            events: mine,
            buf: vec![0; messenger::DEFAULT_BUFFER],
        };
        client
            .radio
            .attach(theirs.handle())
            .map_err(|e| format!("Attach: {}", e.message()))?;
        Ok(client)
    }

    pub fn info(&self) -> Result<wire::HwInfo, String> {
        self.radio.info().map_err(call_text)
    }

    pub fn set_country(&self, alpha2: &str) -> Result<(), String> {
        self.radio.set_country(alpha2).map_err(call_text)
    }

    pub fn scan(&self, request: wire::ScanRequest) -> Result<u32, String> {
        self.radio.scan(&request).map_err(call_text)
    }

    /// Every result of `scan_id`, page by page.
    pub fn scan_results(&self, scan_id: u32) -> Result<Vec<wire::ScanEntry>, String> {
        let mut all = Vec::new();
        loop {
            let page = self
                .radio
                .scan_results(scan_id, all.len() as u32, 64)
                .map_err(call_text)?;
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
        self.radio.join(&request).map_err(call_text)
    }

    pub fn tx(&self, kind: u32, frame: Vec<u8>) -> Result<(), String> {
        self.radio.tx_mgmt(kind, &frame).map_err(call_text)
    }

    pub fn set_state(&self, state: u32, aid: u32) -> Result<(), String> {
        self.radio.set_state(state, aid).map_err(call_text)
    }

    pub fn leave(&self, reason: u32) -> Result<(), String> {
        self.radio.leave(reason).map_err(call_text)
    }

    pub fn set_key(
        &self,
        kind: u32,
        index: u32,
        key: &[u8],
        rsc: &[u8],
        addr: &[u8; 6],
    ) -> Result<(), String> {
        self.radio
            .set_key(kind, index, wire::CIPHER_CCMP128, key, rsc, addr)
            .map_err(call_text)
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
            Err(error) if error.errno() == Some(-user::messenger::errno::ETIMEDOUT) => {
                return Ok(None)
            }
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
