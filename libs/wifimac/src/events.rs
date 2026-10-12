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

/// Events waiting for the client.
pub const EVENT_QUEUE: usize = 64;
/// Of those, at most this many `RxMgmt`: the rest is reserved for the
/// control events (`ScanDone`, `BeaconLoss`, `Deauthenticated`), which a
/// flood of frames from the air must never push out.
pub const RXMGMT_QUEUE: usize = 48;

impl Event {
    fn is_frame(&self) -> bool {
        matches!(self, Event::RxMgmt { .. })
    }

    /// A repeat of this control event while the client has not seen the
    /// first tells it nothing new (it acts on the first and ignores the
    /// repeat): it is folded into the one queued.
    fn folds_into(&self, queued: &Event) -> bool {
        matches!(
            (self, queued),
            (Event::BeaconLoss, Event::BeaconLoss)
                | (Event::Deauthenticated { .. }, Event::Deauthenticated { .. })
        )
    }
}

/// What [`Events::push`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pushed {
    /// The event is in the queue (or folded into an equal one there).
    pub queued: bool,
    /// Events lost: the new one, or a queued `RxMgmt` evicted for it.
    pub dropped: u64,
}

/// A bounded queue that never grows past [`EVENT_QUEUE`]: frames are dropped
/// (newest first) past [`RXMGMT_QUEUE`], and a control event that finds the
/// queue full evicts the oldest frame instead of being lost.
#[derive(Debug, Default)]
pub struct Events {
    queue: VecDeque<Event>,
    frames: usize,
}

impl Events {
    /// Queue `event` (see the type notes); the caller counts what was lost.
    pub fn push(&mut self, event: Event) -> Pushed {
        if event.is_frame() {
            if self.frames >= RXMGMT_QUEUE || self.queue.len() >= EVENT_QUEUE {
                return Pushed {
                    queued: false,
                    dropped: 1,
                };
            }
            self.frames += 1;
            self.queue.push_back(event);
            return Pushed {
                queued: true,
                dropped: 0,
            };
        }
        let last_control = self.queue.iter().rev().find(|e| !e.is_frame());
        if last_control.is_some_and(|queued| event.folds_into(queued)) {
            return Pushed {
                queued: true,
                dropped: 0,
            };
        }
        let mut dropped = 0;
        if self.queue.len() >= EVENT_QUEUE {
            let Some(oldest) = self.queue.iter().position(Event::is_frame) else {
                return Pushed {
                    queued: false,
                    dropped: 1,
                };
            };
            self.queue.remove(oldest);
            self.frames -= 1;
            dropped = 1;
        }
        self.queue.push_back(event);
        Pushed {
            queued: true,
            dropped,
        }
    }

    pub fn pop(&mut self) -> Option<Event> {
        let event = self.queue.pop_front()?;
        if event.is_frame() {
            self.frames -= 1;
        }
        Some(event)
    }

    /// Put back an event [`Events::pop`] just took (the client's inbox was
    /// full): it goes first again and is never dropped for it.
    pub fn unpop(&mut self, event: Event) {
        if event.is_frame() {
            self.frames += 1;
        }
        self.queue.push_front(event);
    }

    pub fn clear(&mut self) {
        self.queue.clear();
        self.frames = 0;
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
    /// Events dropped (a full queue, the `RxMgmt` rate limit) and
    /// management frames refused for size.
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
