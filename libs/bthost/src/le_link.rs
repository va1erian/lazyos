//! What an LE link has beyond a [`crate::link::Link`]: the SMP machine, the
//! key material waiting for admission, the GATT driver.

use alloc::vec::Vec;

use btbond::{AddrKind, ReportHandle};
use btgatt::Hogp;
use bthci::BdAddr;
use btsmp::Central;

/// Key material from a pairing, held until the device has shown it is an
/// acceptable HID device.
pub(crate) struct PendingKey {
    pub ltk: [u8; 16],
    pub key_size: u8,
    pub authenticated: bool,
}

/// The identity a pairing produced, before GATT discovery.
pub(crate) struct Fresh {
    pub key: PendingKey,
    pub irk: Option<[u8; 16]>,
    pub kind: AddrKind,
}

/// What an LE link has beyond a [`Link`].
pub(crate) struct LeLink {
    /// The address type the controller reported (0 public, 1 random).
    pub addr_type: u8,
    /// The address used on the air (a resolvable private address, perhaps).
    pub conn_addr: BdAddr,
    pub smp: Option<Central>,
    pub smp_deadline: Option<u64>,
    /// Pairing waits for the controller's address (`Read_BD_ADDR`).
    pub waits_addr: bool,
    pub pending_key: Option<PendingKey>,
    pub fresh: Option<Fresh>,
    pub enc_deadline: Option<u64>,
    /// Data held across the encryption procedure is being replayed: only SMP
    /// may legitimately race the event, ATT in it was sent before the link
    /// was encrypted and is dropped.
    pub replaying: bool,
    pub gatt: Option<Hogp>,
    /// The input reports being listened to, to find a report id by handle.
    pub reports: Vec<ReportHandle>,
}

impl LeLink {
    pub fn new(addr_type: u8, conn_addr: BdAddr) -> LeLink {
        LeLink {
            addr_type,
            conn_addr,
            smp: None,
            smp_deadline: None,
            waits_addr: false,
            pending_key: None,
            fresh: None,
            enc_deadline: None,
            replaying: false,
            gatt: None,
            reports: Vec::new(),
        }
    }

    pub fn deadline(&self) -> Option<u64> {
        [
            self.smp_deadline,
            self.enc_deadline,
            self.gatt.as_ref().and_then(Hogp::deadline),
        ]
        .into_iter()
        .flatten()
        .min()
    }
}

/// An LE advertisement that offers HID: the HID appearance category, or the
/// HID service among the 16-bit service UUIDs.
pub(crate) fn is_hid_adv(data: &[u8]) -> bool {
    let mut rest = data;
    while let Some((&len, tail)) = rest.split_first() {
        let len = usize::from(len);
        if len == 0 || len > tail.len() {
            return false;
        }
        let (field, next) = tail.split_at(len);
        rest = next;
        let (kind, value) = (field[0], &field[1..]);
        let hit = match kind {
            // Appearance: category 0x0F (Human Interface Device).
            0x19 => value.len() >= 2 && u16::from_le_bytes([value[0], value[1]]) >> 6 == 0x0F,
            0x02 | 0x03 => value.chunks(2).any(|u| u == [0x12, 0x18]),
            _ => false,
        };
        if hit {
            return true;
        }
    }
    false
}
