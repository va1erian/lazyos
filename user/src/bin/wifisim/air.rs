//! The simulated radio: a [`wifimac::Radio`] whose air is a second
//! virtio-net card. Frames go out and come in as `libs/simair` Ethernet
//! frames through that card's `netdrv` (this program is its one
//! `os.lazy.net.nic.v1` client), and the radio does what a chip does: hears
//! only the channel it is tuned to, filters by address, and runs CCMP in
//! software with the installed keys (decrypting in place and keeping the
//! headers and the MIC, as the RTL8822CE does).

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use framering::{Consumer, FrameBuf, Producer, MAX_FRAME as RING_FRAME};
use ieee80211::frame::{SUB_BEACON, SUB_PROBE_RESP};
use ieee80211::{is_group, Mac};
use simair::ccmp::SoftCcmp;
use simair::Air;
use user::messenger::net::{self as nic_api, Client, Shared};
use user::messenger::{self, errno, Endpoint};
use user::wifi::Hardware;
use wifimac::radio::CIPHER_CCMP128;
use wifimac::{
    Band, Channel, KeyInstall, KeyKind, Radio, RadioError, RadioInfo, Received, StaState, TxOptions,
};

/// Ring slots on the air card.
const SLOTS: u32 = 128;
/// Frames waiting for room in the air card's transmit ring.
const OUTBOUND: usize = 256;
/// Received frames waiting for the soft-MAC.
const INBOUND: usize = 256;
/// Air frames read per pump.
const RX_BUDGET: usize = 256;

pub struct AirRadio {
    client: Client,
    ring: u32,
    rx: Consumer,
    tx: Producer,
    shared: Option<Shared>,
    notify: Endpoint,
    station: Mac,
    tuned: Channel,
    bssid: Option<Mac>,
    scanning: bool,
    keys: SoftCcmp,
    inbound: VecDeque<Received>,
    outbound: VecDeque<Vec<u8>>,
    frame: Box<FrameBuf>,
    scratch: Vec<u8>,
    /// Protected frames that failed decryption (a wrong key or PN on the
    /// other side shows here).
    pub mic_failures: u64,
}

impl AirRadio {
    /// Attach to the air card `ifname`, whose name must be held by a NIC
    /// driver identity.
    pub fn connect(ifname: &str) -> Result<AirRadio, String> {
        let present = nic_api::present().map_err(|e| format!("registry: {}", e.message()))?;
        let card = present
            .iter()
            .find(|card| card.ifname == ifname)
            .ok_or_else(|| format!("no air card {ifname} yet"))?;
        if !card.owner_is_driver() {
            return Err(format!(
                "{ifname} is not held by a NIC driver (uid {})",
                card.owner_uid
            ));
        }
        let client = Client::connect(ifname).map_err(|e| format!("connect: {}", e.message()))?;
        let info = client
            .info()
            .map_err(|e| format!("Info: {}", e.message()))?;
        let station: Mac = info
            .mac
            .as_slice()
            .try_into()
            .map_err(|_| String::from("the air card reported a bad MAC"))?;
        let (mine, theirs) =
            messenger::create_pair().map_err(|e| format!("pair: {}", e.message()))?;
        let attachment = match client.attach_notifying(SLOTS, theirs) {
            Ok(attachment) => attachment,
            Err(error) => {
                let _ = mine.close();
                let _ = theirs.close();
                return Err(format!("AttachRing: {}", error.message()));
            }
        };
        let ring = attachment.ring;
        let (rx, tx, shared) = attachment.split();
        Ok(AirRadio {
            client,
            ring,
            rx,
            tx,
            shared: Some(shared),
            notify: mine,
            station,
            tuned: Channel::new(Band::Ghz2, 1),
            bssid: None,
            scanning: false,
            keys: SoftCcmp::default(),
            inbound: VecDeque::new(),
            outbound: VecDeque::new(),
            frame: Box::new([0; RING_FRAME]),
            scratch: vec![0; 256],
            mic_failures: 0,
        })
    }

    /// Whether the radio would hear `frame` (sent on its channel): to our
    /// address; a group frame of the joined BSS; any beacon or probe response
    /// while scanning.
    fn hears(&self, frame: &[u8]) -> bool {
        let (Some(a1), Some(a2), Some(a3)) =
            (frame.get(4..10), frame.get(10..16), frame.get(16..22))
        else {
            return false;
        };
        if a1 == self.station {
            return true;
        }
        let a1: Mac = a1.try_into().unwrap_or([0; 6]);
        if !is_group(&a1) {
            return false;
        }
        let mgmt = (frame[0] >> 2) & 0x03 == 0;
        let subtype = frame[0] >> 4;
        let beacon = mgmt && (subtype == SUB_BEACON || subtype == SUB_PROBE_RESP);
        let ours = self.bssid.is_some_and(|b| a2 == b || a3 == b);
        (beacon && self.scanning) || ours
    }

    /// One Ethernet frame from the air card.
    fn take(&mut self, eth: &[u8]) {
        let Ok((air, frame)) = simair::decode(eth) else {
            return;
        };
        let band = if air.band == 0 {
            Band::Ghz2
        } else {
            Band::Ghz5
        };
        if band != self.tuned.band || air.channel != self.tuned.number || !self.hears(frame) {
            return;
        }
        let mut frame = Vec::from(frame);
        let protected = frame[1] & 0x40 != 0;
        if protected && self.keys.open(&mut frame).is_err() {
            self.mic_failures += 1;
            return;
        }
        if self.inbound.len() >= INBOUND {
            return;
        }
        self.inbound.push_back(Received {
            frame,
            rssi_dbm: air.rssi_dbm,
            band,
            channel: air.channel,
            decrypted: protected,
            trailer: if protected {
                ieee80211::ccmp::MIC_LEN
            } else {
                0
            },
        });
    }
}

impl Drop for AirRadio {
    fn drop(&mut self) {
        let _ = self.client.detach(self.ring);
        if let Some(shared) = self.shared.take() {
            shared.close();
        }
        let _ = self.notify.close();
    }
}

impl Radio for AirRadio {
    fn info(&self) -> RadioInfo {
        RadioInfo {
            mac: self.station,
            bands: Band::Ghz2.bit() | Band::Ghz5.bit(),
            ciphers: 1 << CIPHER_CCMP128,
            // The MIC the radio appends must still fit the air.
            max_frame: simair::MAX_FRAME - ieee80211::ccmp::MIC_LEN,
            key_slots: 8,
        }
    }

    fn set_address(&mut self, mac: Mac) -> Result<(), RadioError> {
        self.station = mac;
        Ok(())
    }

    fn tune(&mut self, channel: Channel) -> Result<(), RadioError> {
        if channel.band == Band::Ghz6 {
            return Err(RadioError::Invalid);
        }
        self.tuned = channel;
        Ok(())
    }

    fn set_bssid(&mut self, bssid: Option<Mac>, _aid: u16) -> Result<(), RadioError> {
        self.bssid = bssid;
        Ok(())
    }

    fn set_scan_filter(&mut self, scanning: bool) -> Result<(), RadioError> {
        self.scanning = scanning;
        Ok(())
    }

    fn set_state(&mut self, _state: StaState) -> Result<(), RadioError> {
        Ok(())
    }

    fn set_key(&mut self, key: &KeyInstall<'_>) -> Result<(), RadioError> {
        let group = match key.kind {
            KeyKind::Pairwise => false,
            KeyKind::Group => true,
            KeyKind::Igtk => return Err(RadioError::Invalid),
        };
        if key.cipher != CIPHER_CCMP128 || !self.keys.install(group, key.index, key.key) {
            return Err(RadioError::Invalid);
        }
        Ok(())
    }

    fn del_key(&mut self, kind: KeyKind, index: u8) -> Result<(), RadioError> {
        self.keys.remove(kind != KeyKind::Pairwise, index);
        Ok(())
    }

    fn transmit(&mut self, frame: &[u8], options: &TxOptions) -> Result<(), RadioError> {
        if self.outbound.len() >= OUTBOUND {
            return Err(RadioError::Full);
        }
        let sealed;
        let frame = if options.protected {
            sealed = self.keys.seal(frame).map_err(|_| RadioError::Invalid)?;
            &sealed[..]
        } else {
            frame
        };
        let air = Air {
            band: self.tuned.band as u8,
            channel: self.tuned.number,
            rssi_dbm: 0,
            flags: if options.basic_rate {
                simair::FLAG_BASIC_RATE
            } else {
                0
            },
        };
        let eth = simair::encode(&self.station, &air, frame).map_err(|_| RadioError::Invalid)?;
        self.outbound.push_back(eth);
        Ok(())
    }

    fn tx_ready(&self) -> bool {
        self.outbound.len() < OUTBOUND
    }

    fn receive(&mut self) -> Option<Received> {
        self.inbound.pop_front()
    }
}

impl Hardware for AirRadio {
    fn wake_endpoint(&self) -> Option<Endpoint> {
        Some(self.notify)
    }

    fn pump(&mut self, _now_ms: u64) -> Result<(), String> {
        // The driver's notices only say "look at the rings": drain them.
        loop {
            match self.notify.poll_recv_with(&mut self.scratch) {
                Ok(Some(_)) => {}
                Ok(None) => break,
                Err(error) if error.errno() == Some(-errno::EPIPE) => {
                    return Err(String::from("the air card's driver went away"));
                }
                Err(_) => break,
            }
        }
        for _ in 0..RX_BUDGET {
            match self.rx.pop(&mut self.frame) {
                Ok(Some(len)) => {
                    // Copied out of the shared ring before it is looked at.
                    let eth = self.frame[..len].to_vec();
                    self.take(&eth);
                }
                Ok(None) => {
                    self.rx.arm();
                    if self
                        .rx
                        .pending()
                        .map_err(|_| String::from("air receive ring corrupt"))?
                        == 0
                    {
                        break;
                    }
                }
                Err(framering::PopError::BadLength(_)) => {}
                Err(framering::PopError::Corrupt) => {
                    return Err(String::from("air receive ring corrupt"));
                }
            }
        }
        let mut pushed = false;
        while let Some(eth) = self.outbound.front() {
            match self.tx.push(eth) {
                Ok(()) => {
                    pushed = true;
                    self.outbound.pop_front();
                }
                Err(framering::PushError::Full) => break,
                Err(_) => {
                    // A frame the ring refuses (it cannot happen for an air
                    // frame under the card's MTU) is dropped, not retried.
                    self.outbound.pop_front();
                }
            }
        }
        if pushed && self.client.kick(self.ring).is_err() {
            return Err(String::from("the air card's driver went away (kick)"));
        }
        Ok(())
    }
}
