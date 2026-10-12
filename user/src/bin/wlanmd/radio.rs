//! One radio: its `os.lazy.net.wifi.hw.v1` client, the event channel it
//! sends on, and the station that runs it. Executes the station's actions
//! against the radio and feeds back what they yield.

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use user::central::Bus;
use user::messenger::wifi::{api, hw, Radio};
use user::messenger::{self, errno, Endpoint, Error, Message};
use user::sys;
use wlansta::profile::WORLD;
use wlansta::{Action, Event, FrameKind, HwOp, ScanEntry, Station, Status, Viewer};

use crate::store::Store;

/// Ticks one radio call may take before the driver is considered wedged.
const CALL_TICKS: u64 = 300;
/// Room for one `ScanResults` page.
const SCAN_BUFFER: usize = 64 * 1024;
/// Events read from one radio per wake, so one chatty radio cannot starve
/// the service.
const EVENTS_PER_WAKE: usize = 32;

pub(crate) type Sta = Station<fn() -> [u8; 32]>;

pub(crate) struct Slot {
    pub ifname: String,
    pub mac: [u8; 6],
    radio: Radio,
    /// Our end of the event channel the driver sends on.
    pub events: Endpoint,
    pub station: Sta,
    /// The regulatory domain the radio accepted.
    pub country: String,
    /// Tick the registry last listed it.
    pub seen: u64,
    published_changes: Option<u32>,
    published_generation: u32,
    printed_changes: Option<u32>,
    printed_generation: u32,
    buffer: Vec<u8>,
}

pub(crate) fn mac_text(mac: &[u8; 6]) -> String {
    let [a, b, c, d, e, f] = *mac;
    format!("{a:02x}:{b:02x}:{c:02x}:{d:02x}:{e:02x}:{f:02x}")
}

impl Slot {
    /// Resolve the radio `ifname`, read its address, attach with a fresh
    /// event channel and set the regulatory domain (the world domain when
    /// the radio refuses `country`; no attach when it refuses that too).
    pub fn attach(ifname: &str, country: &str, nonce: fn() -> [u8; 32]) -> messenger::Result<Slot> {
        let radio = Radio::connect(ifname, CALL_TICKS)?;
        let mac: [u8; 6] = match radio.info() {
            Ok(info) => match info.mac.as_slice().try_into() {
                Ok(mac) => mac,
                Err(_) => {
                    radio.release();
                    return Err(Error::Errno(-errno::EINVAL));
                }
            },
            Err(error) => {
                radio.release();
                return Err(error);
            }
        };
        let (mine, theirs) = messenger::create_pair()?;
        if let Err(error) = radio.attach(theirs) {
            let _ = mine.close();
            let _ = theirs.close();
            radio.release();
            return Err(error);
        }
        let country = match set_country(&radio, ifname, country) {
            Ok(code) => code,
            Err(error) => {
                let _ = radio.detach();
                let _ = mine.close();
                radio.release();
                return Err(error);
            }
        };
        Ok(Slot {
            ifname: String::from(ifname),
            mac,
            radio,
            events: mine,
            station: Station::new(mac, nonce),
            country,
            seen: sys::clock(),
            published_changes: None,
            published_generation: 0,
            printed_changes: None,
            printed_generation: 0,
            buffer: vec![0; SCAN_BUFFER],
        })
    }

    /// Give the radio a new regulatory domain (the world domain if it
    /// refuses that one). `Err` when it takes neither: the caller drops it.
    pub fn apply_country(&mut self, country: &str) -> messenger::Result<()> {
        self.country = set_country(&self.radio, &self.ifname, country)?;
        Ok(())
    }

    /// Detach and release everything (the driver may already be gone).
    pub fn close(self) {
        let _ = self.radio.leave(u32::from(wlansta::reason::LEAVING));
        let _ = self.radio.detach();
        let _ = self.events.close();
        self.radio.release();
    }

    /// Run `actions` in order; a radio call that fails stops the batch and is
    /// reported to the station, whose answer runs next. A failed `Leave` or
    /// `AbortScan` is reported but does not stop the batch: the radio leaves
    /// and stops on its own when it can, and what follows (a new join, the
    /// next scan) must still happen.
    pub fn execute(&mut self, store: &mut Store, actions: Vec<Action>) {
        for action in actions {
            match self.run(store, action) {
                Ok(Feedback::None) => {}
                Ok(Feedback::Event(event)) => {
                    let more = self.station.input(crate::now_ms(), event);
                    self.execute(store, more);
                }
                Ok(Feedback::Actions(more)) => self.execute(store, more),
                Err((op, errno)) => {
                    let event = Event::HwError { op, errno };
                    let more = self.station.input(crate::now_ms(), event);
                    self.execute(store, more);
                    if !matches!(op, HwOp::Leave | HwOp::AbortScan) {
                        return;
                    }
                }
            }
        }
    }

    fn run(&mut self, store: &mut Store, action: Action) -> Result<Feedback, (HwOp, i64)> {
        let fail = |op: HwOp| move |error: Error| (op, error.errno().unwrap_or(-errno::EIO));
        match action {
            Action::Scan { ssids } => {
                let scan_id = self.radio.scan(ssids).map_err(fail(HwOp::Scan))?;
                Ok(Feedback::Event(Event::ScanStarted { scan_id }))
            }
            Action::AbortScan { scan_id } => {
                self.radio
                    .abort_scan(scan_id)
                    .map_err(fail(HwOp::AbortScan))?;
                Ok(Feedback::None)
            }
            Action::Join {
                bssid,
                channel,
                band,
            } => {
                self.radio
                    .join(bssid, channel, band as u32)
                    .map_err(fail(HwOp::Join))?;
                Ok(Feedback::None)
            }
            Action::Tx { kind, frame } => {
                self.radio
                    .tx_mgmt(kind as u32, frame)
                    .map_err(fail(HwOp::Tx))?;
                Ok(Feedback::None)
            }
            Action::SetState { state, aid } => {
                self.radio
                    .set_state(state as u32, u32::from(aid))
                    .map_err(fail(HwOp::SetState))?;
                Ok(Feedback::None)
            }
            Action::SetKey {
                kind,
                index,
                key,
                rsc,
                addr,
            } => {
                self.radio
                    .set_key(kind as u32, u32::from(index), key.expose(), &rsc, &addr)
                    .map_err(fail(HwOp::SetKey))?;
                Ok(Feedback::None)
            }
            Action::Leave { reason } => {
                self.radio
                    .leave(u32::from(reason))
                    .map_err(fail(HwOp::Leave))?;
                Ok(Feedback::None)
            }
            Action::FetchPmk { id, owner, ssid } => {
                let answer = store.pmk(&id, owner, &ssid);
                Ok(Feedback::Actions(self.station.pmk(
                    crate::now_ms(),
                    owner,
                    &id,
                    answer,
                )))
            }
        }
    }

    /// Read the waiting events. `false` when the driver is gone.
    pub fn drain_events(&mut self, store: &mut Store) -> bool {
        let mut buffer = core::mem::take(&mut self.buffer);
        let mut alive = true;
        for _ in 0..EVENTS_PER_WAKE {
            match self.events.poll_recv_with(&mut buffer) {
                Ok(Some(message)) => {
                    if let Some(event) = self.event_of(&message, &mut buffer) {
                        let actions = self.station.input(crate::now_ms(), event);
                        self.buffer = core::mem::take(&mut buffer);
                        self.execute(store, actions);
                        buffer = core::mem::take(&mut self.buffer);
                    }
                }
                Ok(None) => break,
                Err(Error::Errno(code)) if code == -errno::EPIPE => {
                    alive = false;
                    break;
                }
                Err(_) => break,
            }
        }
        self.buffer = buffer;
        alive
    }

    /// Decode one driver event; anything else on the channel is ignored.
    fn event_of(&mut self, message: &Message, buffer: &mut [u8]) -> Option<Event> {
        let cred = message.caller();
        let driver = netpolicy::may_register_wifi_hw_name(cred.uid, cred.label_id, cred.session);
        if !driver || message.interface_id() != hw::INTERFACE_ID {
            return None;
        }
        let body = &message.parcel.body;
        match message.method() {
            hw::METHOD_SCANDONE => {
                let args = hw::decode_scan_done_args(body).ok()?;
                let entries = match self.radio.scan_results(args.scan_id, buffer) {
                    Ok(entries) => entries.into_iter().map(entry).collect(),
                    Err(_) => Vec::new(),
                };
                Some(Event::ScanDone {
                    scan_id: args.scan_id,
                    aborted: args.aborted,
                    entries,
                })
            }
            hw::METHOD_RXMGMT => {
                let args = hw::decode_rx_mgmt_args(body).ok()?;
                Some(Event::Rx {
                    kind: FrameKind::from_wire(args.kind)?,
                    rssi_dbm: args.rssi_dbm,
                    frame: args.frame,
                })
            }
            hw::METHOD_BEACONLOSS => Some(Event::BeaconLoss),
            hw::METHOD_DEAUTHENTICATED => {
                let args = hw::decode_deauthenticated_args(body).ok()?;
                Some(Event::Deauthenticated {
                    reason: u16::try_from(args.reason).unwrap_or(u16::MAX),
                })
            }
            _ => None,
        }
    }

    /// Publish the state and the scan generation if they moved (and print
    /// them once each).
    pub fn publish(&mut self, bus: Option<&mut Bus>) {
        let status = self.station.status().clone();
        let generation = self.station.scan_generation();
        if self.printed_changes != Some(status.changes) {
            self.printed_changes = Some(status.changes);
            sys::write_str(&format!(
                "WLANMD:STATE {} {} reason={} ssid=\"{}\"\n",
                self.ifname,
                status.state.name(),
                status.reason.name(),
                status.ssid
            ));
        }
        let count = self.station.networks(wlansta::Viewer::System).len() as u32;
        if self.printed_generation != generation {
            self.printed_generation = generation;
            sys::write_str(&format!(
                "WLANMD:SCAN {} gen={generation} networks={count}\n",
                self.ifname
            ));
        }
        let Some(bus) = bus else {
            return;
        };
        // Everyone may read the topic: a user network's profile id is not
        // published there (its owner reads it through `Status`).
        let public = wire_state(&status, Viewer::System);
        if self.published_changes != Some(status.changes)
            && api::publish_system_net_wifi_state(bus, &self.ifname, &public).is_ok()
        {
            self.published_changes = Some(status.changes);
        }
        if generation != self.published_generation {
            let event = api::ScanEvent { generation, count };
            if api::publish_system_net_wifi_scan(bus, &self.ifname, &event).is_ok() {
                self.published_generation = generation;
            }
        }
    }
}

enum Feedback {
    None,
    Event(Event),
    Actions(Vec<Action>),
}

fn entry(e: hw::ScanEntry) -> ScanEntry {
    ScanEntry {
        frame: e.frame,
        channel: e.channel,
        band: e.band,
        rssi_dbm: e.rssi_dbm,
        age_ms: e.age_ms,
    }
}

/// Set `country` on `radio`, else the world domain; the code it took.
fn set_country(radio: &Radio, ifname: &str, country: &str) -> messenger::Result<String> {
    match radio.set_country(country) {
        Ok(()) => return Ok(String::from(country)),
        Err(error) => sys::write_str(&format!(
            "WLANMD:RADIO:COUNTRY {ifname} {country} {}\n",
            error.message()
        )),
    }
    radio.set_country(WORLD)?;
    Ok(String::from(WORLD))
}

/// The station's status as `WifiState`, as `viewer` may see it: the profile
/// id only when the viewer may see that owner's profiles.
pub(crate) fn wire_state(status: &Status, viewer: Viewer) -> api::WifiState {
    let shown = status.owner.is_some_and(|owner| viewer.sees(owner));
    api::WifiState {
        state: status.state as u32,
        ssid: status.ssid.clone(),
        bssid: status.bssid.map(|b| b.to_vec()).unwrap_or_default(),
        rssi_dbm: status.rssi_dbm,
        freq_mhz: status.freq_mhz,
        id: if shown {
            status.id.clone()
        } else {
            String::new()
        },
        reason: status.reason as u32,
        changes: status.changes,
    }
}
