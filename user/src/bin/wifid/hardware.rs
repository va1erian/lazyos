//! The RTL8822CE as a `wifimac::Radio` (docs/wifi-rtw88-plan.md section 3):
//! what `user::wifi::run` needs to serve `os.lazy.net.wifi.hw/<if>` and
//! `os.lazy.net.nic/<if>` over `libs/rtw88`'s station facade.
//!
//! | `Radio` | `rtw88::Dev` (upstream) |
//! |---|---|
//! | `tune` | `set_channel` (`rtw_set_channel`), 20 MHz |
//! | `set_scan_filter` | `scan_start` / `scan_complete` (`rtw_core_scan_*`) |
//! | `set_bssid` | `set_bssid` (`BSS_CHANGED_BSSID`) |
//! | `set_state` | `assoc` / `disassoc` (`BSS_CHANGED_ASSOC`, `sta_add`) |
//! | `set_key` / `del_key` | `set_key` / `del_key` (`rtw_ops_set_key`, the CAM) |
//! | `transmit` | `tx_mgmt` / `tx_data` + `send` (`rtw_tx`) |
//!
//! **Encryption** follows the trait's contract, which is this chip's: a
//! protected frame arrives with its CCMP header built by the soft-MAC and is
//! sent with the CAM entry of its key (pairwise for an individually
//! addressed frame, the group key id otherwise), the chip appending the MIC;
//! a received frame the chip decrypted keeps both headers and its 8-octet
//! MIC (`RCR_APP_MIC`), reported as `trailer`. The FCS (`RCR_APP_FCS`) is
//! cut here.
//!
//! **Rates.** The station's association request carries legacy rates only
//! (`wlanmd` today), so the firmware's rate adaptation is told the band's
//! legacy set (`PeerCaps`), at 20 MHz.

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec::Vec;

use rtw88::dev::{Band as RtwBand, Bw};
use rtw88::station::{Cipher, PeerCaps};
use rtw88::Error;
use user::messenger::Endpoint;
use wifimac::radio::CIPHER_CCMP128;
use wifimac::{
    Band, Channel, KeyInstall, KeyKind, Radio, RadioError, RadioInfo, Received, StaState,
    TxOptions,
};

use super::radio::Chip;
use super::{device, trace};

/// Frames held between the chip and the soft-MAC; past this the oldest go.
const RX_QUEUE: usize = 256;
/// RX buffers per poll before checking the interrupt channel again.
const RX_BUDGET: u32 = 64;
/// The CCMP MIC the chip leaves after a frame it decrypted.
const MIC_LEN: usize = 8;
const FCS_LEN: usize = 4;
/// Largest frame handed to `transmit`: a TX slot (2 KiB) less the 48-octet
/// descriptor, the chip appending MIC and FCS on the air.
const MAX_FRAME: usize = 2000;
/// Legacy rates (`PeerCaps` bit order): every rate of the band, and the
/// mandatory ones as basic.
const RATES_2G: (u32, u32) = (0x0fff, 0x000f);
const RATES_5G: (u32, u32) = (0x00ff, 0x0015);

/// One installed key: what the soft-MAC calls it and where the CAM put it.
#[derive(Clone, Copy)]
struct Slot {
    kind: KeyKind,
    index: u8,
    cam: u8,
}

/// The chip, its interrupt channel and what the soft-MAC needs remembered.
pub(super) struct Rtw {
    dev: Chip,
    mac: [u8; 6],
    irq: Option<(Endpoint, u64)>,
    rx: VecDeque<Received>,
    keys: Vec<Slot>,
    channel: Channel,
    bssid: Option<[u8; 6]>,
    aid: u16,
    scanning: bool,
    buffer: [u8; 256],
    /// Frames dropped: queue overflow, CRC/ICV errors.
    dropped: u64,
}

fn radio_error(error: Error) -> RadioError {
    match error {
        Error::Full(_) | Error::NoMemory => RadioError::Full,
        Error::Invalid(_) | Error::Unsupported(_) => RadioError::Invalid,
        _ => RadioError::Hardware,
    }
}

impl Rtw {
    pub(super) fn new(dev: Chip, mac: [u8; 6], irq: Option<(Endpoint, u64)>) -> Rtw {
        Rtw {
            dev,
            mac,
            irq,
            rx: VecDeque::new(),
            keys: Vec::new(),
            channel: Channel::new(Band::Ghz2, 1),
            bssid: None,
            aid: 0,
            scanning: false,
            buffer: [0; 256],
            dropped: 0,
        }
    }

    /// The CAM entry a protected frame to `addr1` is encrypted with.
    fn tx_key(&self, frame: &[u8]) -> Option<u8> {
        let group = frame.get(4).is_some_and(|b| b & 1 != 0);
        // The CCMP header's key id octet: bits 6-7 of its fourth octet.
        let key_id = frame.get(header_len(frame) + 3).map(|b| b >> 6)?;
        self.keys
            .iter()
            .find(|s| {
                if group {
                    s.kind == KeyKind::Group && s.index == key_id
                } else {
                    s.kind == KeyKind::Pairwise
                }
            })
            .map(|s| s.cam)
    }

    /// Move every waiting RX buffer to the queue (C2H to the chip library).
    fn drain_rx(&mut self) {
        loop {
            let mut c2h: Vec<Vec<u8>> = Vec::new();
            let (rx, dropped, channel) = (&mut self.rx, &mut self.dropped, self.channel);
            let poll = self.dev.hci.rx_poll(RX_BUDGET, |p| {
                if p.stat.is_c2h {
                    c2h.push(p.payload().to_vec());
                    return;
                }
                let payload = p.payload();
                if p.stat.crc_err || p.stat.icv_err || payload.len() <= FCS_LEN {
                    *dropped += 1;
                    return;
                }
                if rx.len() >= RX_QUEUE {
                    rx.pop_front();
                    *dropped += 1;
                }
                let power = p.stat.rx_power[0].max(p.stat.rx_power[1]);
                rx.push_back(Received {
                    frame: payload[..payload.len() - FCS_LEN].to_vec(),
                    rssi_dbm: power,
                    band: channel.band,
                    channel: channel.number,
                    decrypted: p.stat.decrypted,
                    trailer: if p.stat.decrypted { MIC_LEN } else { 0 },
                });
            });
            for pkt in c2h {
                let _ = self.dev.handle_c2h(&pkt);
            }
            if !poll.more {
                break;
            }
        }
    }
}

/// Octets of the MAC header of a data frame (24, +6 with four addresses,
/// +2 with QoS); 24 for anything shorter or not data.
fn header_len(frame: &[u8]) -> usize {
    let (Some(&fc0), Some(&fc1)) = (frame.first(), frame.get(1)) else {
        return 24;
    };
    let mut len = 24;
    if (fc0 >> 2) & 3 == 2 {
        if fc1 & 3 == 3 {
            len += 6;
        }
        if fc0 & 0x80 != 0 {
            len += 2;
        }
    }
    len
}

impl Radio for Rtw {
    fn info(&self) -> RadioInfo {
        RadioInfo {
            mac: self.mac,
            bands: Band::Ghz2.bit() | Band::Ghz5.bit(),
            ciphers: 1 << CIPHER_CCMP128,
            max_frame: MAX_FRAME,
            key_slots: u32::from(self.dev.sec.total_cam_num),
        }
    }

    fn set_address(&mut self, mac: [u8; 6]) -> Result<(), RadioError> {
        self.mac = mac;
        self.dev.set_port(mac);
        Ok(())
    }

    fn tune(&mut self, channel: Channel) -> Result<(), RadioError> {
        let ok = match channel.band {
            Band::Ghz2 => (1..=14).contains(&channel.number),
            Band::Ghz5 => (36..=177).contains(&channel.number),
            Band::Ghz6 => false,
        };
        if !ok {
            return Err(RadioError::Invalid);
        }
        let n = channel.number;
        self.dev.set_channel(n, Bw::Mhz20, n).map_err(radio_error)?;
        self.channel = channel;
        Ok(())
    }

    fn set_bssid(&mut self, bssid: Option<[u8; 6]>, aid: u16) -> Result<(), RadioError> {
        self.dev.set_bssid(bssid.unwrap_or([0; 6]));
        self.bssid = bssid;
        self.aid = aid;
        Ok(())
    }

    fn set_scan_filter(&mut self, scanning: bool) -> Result<(), RadioError> {
        if scanning == self.scanning {
            return Ok(());
        }
        self.scanning = scanning;
        if scanning {
            self.dev.scan_start(self.mac)
        } else {
            self.dev.scan_complete()
        }
        .map_err(radio_error)
    }

    fn set_state(&mut self, state: StaState) -> Result<(), RadioError> {
        match state {
            StaState::Idle | StaState::Authenticated => self.dev.disassoc(),
            StaState::Associated => {
                let (supp_rates, basic_rates) = match self.dev.band() {
                    RtwBand::Ghz2 => RATES_2G,
                    RtwBand::Ghz5 => RATES_5G,
                };
                let peer = PeerCaps {
                    supp_rates,
                    basic_rates,
                    ht: None,
                    vht: None,
                    bw: Bw::Mhz20,
                };
                self.dev.assoc(self.aid, &peer)
            }
            // The keys are in; nothing more for the firmware.
            StaState::Authorized => Ok(()),
        }
        .map_err(radio_error)
    }

    fn set_key(&mut self, key: &KeyInstall<'_>) -> Result<(), RadioError> {
        if key.cipher != CIPHER_CCMP128 || key.kind == KeyKind::Igtk {
            return Err(RadioError::Invalid);
        }
        let _ = self.del_key(key.kind, key.index);
        let peer = (key.kind == KeyKind::Pairwise).then_some(key.peer);
        let cam = self
            .dev
            .set_key(Cipher::Ccmp, key.index, peer, key.key)
            .map_err(radio_error)?;
        self.keys.push(Slot {
            kind: key.kind,
            index: key.index,
            cam,
        });
        Ok(())
    }

    fn del_key(&mut self, kind: KeyKind, index: u8) -> Result<(), RadioError> {
        let Some(at) = self
            .keys
            .iter()
            .position(|s| s.kind == kind && s.index == index)
        else {
            return Ok(());
        };
        let slot = self.keys.remove(at);
        self.dev.del_key(slot.cam).map_err(radio_error)
    }

    fn transmit(&mut self, frame: &[u8], options: &TxOptions) -> Result<(), RadioError> {
        if frame.len() > MAX_FRAME {
            return Err(RadioError::Invalid);
        }
        // Upstream calibrates on the first transmission on an operating
        // channel, not on every scan channel (`need_rfk`).
        if !self.scanning {
            self.dev.prepare_tx();
        }
        let built = if options.data {
            let key = if options.protected {
                Some(self.tx_key(frame).ok_or(RadioError::Invalid)?)
            } else {
                None
            };
            self.dev.tx_data(frame, options.priority, key)
        } else {
            self.dev.tx_mgmt(frame)
        }
        .map_err(radio_error)?;
        self.dev.send(&built).map_err(radio_error)
    }

    fn tx_ready(&self) -> bool {
        use rtw88::hci::TxQueue;
        [TxQueue::Mgmt, TxQueue::Be]
            .iter()
            .all(|&q| self.dev.hci.tx_avail(q) > 0)
    }

    fn receive(&mut self) -> Option<Received> {
        self.rx.pop_front()
    }
}

impl user::wifi::Hardware for Rtw {
    fn wake_endpoint(&self) -> Option<Endpoint> {
        self.irq.map(|(channel, _)| channel)
    }

    fn pump(&mut self, _now_ms: u64) -> Result<(), String> {
        if let Some((channel, handle)) = self.irq {
            while let Ok(Some(message)) = channel.poll_recv_with(&mut self.buffer) {
                if device::is_interrupt(&message) {
                    let events = self.dev.hci.interrupt();
                    let _ = user::dev::irq_ack(handle);
                    if events.tx_fifo_overflow {
                        trace::event("TXFOVW", &[("dropped", &self.dropped)]);
                    }
                }
            }
        }
        // Drain whether or not a message came: a buffer that landed while
        // RX was masked raises none (`rx_poll`'s `more`).
        if self.dev.hci.rx_pending() != 0 {
            self.drain_rx();
        }
        Ok(())
    }
}
