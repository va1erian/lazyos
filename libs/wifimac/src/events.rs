//! What the soft-MAC tells its client (`os.lazy.net.wifi.hw.v1`'s one-way
//! events) and the counters it keeps.

use alloc::collections::VecDeque;
use alloc::vec::Vec;

/// `FrameKind` ordinals of `TxMgmt` and `RxMgmt`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameKind {
    /// A complete 802.11 management frame.
    Mgmt = 0,
    /// An EAPOL frame as 802.3 (destination, source, 0x888E, PDU).
    Eapol = 1,
}

impl FrameKind {
    pub fn from_u32(value: u32) -> Option<FrameKind> {
        match value {
            0 => Some(FrameKind::Mgmt),
            1 => Some(FrameKind::Eapol),
            _ => None,
        }
    }
}

/// One event for the attached client, in the order it happened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    ScanDone {
        scan_id: u32,
        aborted: bool,
    },
    /// A management frame or an EAPOL frame from the joined BSS; the bytes
    /// are unparsed and hostile.
    RxMgmt {
        kind: FrameKind,
        rssi_dbm: i32,
        frame: Vec<u8>,
    },
    BeaconLoss,
    Deauthenticated {
        reason: u16,
    },
}

/// Events waiting for the client before the oldest is dropped.
pub const EVENT_QUEUE: usize = 64;

/// A bounded queue: an event that does not fit is dropped and counted, never
/// grows the queue.
#[derive(Debug, Default)]
pub struct Events {
    queue: VecDeque<Event>,
}

impl Events {
    /// Queue `event`; false when the queue is full (the caller counts it).
    pub fn push(&mut self, event: Event) -> bool {
        if self.queue.len() >= EVENT_QUEUE {
            return false;
        }
        self.queue.push_back(event);
        true
    }

    pub fn pop(&mut self) -> Option<Event> {
        self.queue.pop_front()
    }

    pub fn clear(&mut self) {
        self.queue.clear();
    }

    pub fn len(&self) -> usize {
        self.queue.len()
    }

    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }
}

/// Counters since the soft-MAC started: `HwStats` plus the data path.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub scans: u64,
    pub mgmt_rx: u64,
    pub mgmt_tx: u64,
    /// Events dropped (a full queue) and management frames refused for size.
    pub mgmt_dropped: u64,
    pub beacon_losses: u64,
    pub deauths: u64,
    /// Radio operations that failed with a hardware error.
    pub fw_errors: u64,
    pub resets: u64,
    /// Data frames received and passed up, and sent.
    pub rx_data: u64,
    pub tx_data: u64,
    /// Data frames dropped on receive: not from the BSS, not decrypted, port
    /// closed, malformed, queue full.
    pub rx_data_dropped: u64,
    /// Protected frames whose packet number was not above the last one.
    pub replays: u64,
    /// Data frames refused on transmit (port closed, too long, radio full).
    pub tx_data_dropped: u64,
}
