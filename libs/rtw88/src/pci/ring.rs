//! Where the rings live in DMA memory, the ring indices, and the only code
//! that touches buffer-descriptor memory (upstream `pci.h`:
//! `struct rtw_pci_ring`, `avail_desc`, the TX and RX buffer descriptors).
//!
//! **DMA budget.** Upstream allocates one coherent block per ring and one
//! skb per RX descriptor (512 of them, 11 KiB each). The kernel gives a
//! claim 16 blocks of at most 4 MiB, 8 MiB per uid, so this driver takes
//! three blocks and sizes the rings for a station, not an access point:
//!
//! | Block | Holds | Default size |
//! |---|---|---|
//! | `desc` | every buffer-descriptor ring, each on its own page | 36 KiB |
//! | `txbuf` | one slot per TX descriptor: 2 KiB each, 8 KiB for the beacon queue | 392 KiB |
//! | `rxbuf` | one 12 KiB slot per RX descriptor (`RTK_PCI_RX_BUF_SIZE` = 11478) | 1.5 MiB |
//!
//! about 1.9 MiB in all with [`RingSizes::DEFAULT`]: TX BK 16, BE 64, VI 16,
//! VO 16, BCN 1, MGMT 32, HI0 16, H2C 32 descriptors; RX 128 descriptors.
//! The device only ever sees driver-chosen addresses inside these blocks; a
//! slot is never re-pointed.

use core::ptr;

use crate::error::{Error, Result};
use crate::hci::TxQueue;
use crate::rtw8822c::spec::{RX_BUF_DESC_SZ, TX_BUF_DESC_SZ, TX_PKT_DESC_SZ};

use super::regs::TRX_BD_IDX_MASK;
use super::DmaBuf;

const PAGE: usize = 4096;
/// The largest block the kernel hands one claim.
pub const MAX_DMA_BLOCK: usize = 4 << 20;
/// Bytes the chip may write per RX descriptor (`RTK_PCI_RX_BUF_SIZE`: an
/// 11454-byte VHT MPDU plus the RX descriptor).
pub const RX_BUF_SIZE: usize = 11454 + 24;
/// Stride of RX slots: [`RX_BUF_SIZE`] rounded up to pages.
pub const RX_SLOT: usize = 12288;
const _: () = assert!(RX_SLOT >= RX_BUF_SIZE && RX_SLOT.is_multiple_of(PAGE));
/// Stride of TX slots: descriptor and an 802.11 frame of a 1500-byte payload
/// with QoS header, LLC/SNAP and CCMP header fit with room to spare.
pub const TX_SLOT: usize = 2048;
/// The beacon queue's one slot: a 4 KiB firmware chunk plus descriptor
/// (`download_firmware_to_mem` sends 0x1000 bytes at a time).
pub const BCN_SLOT: usize = 8192;

/// Descriptors per ring.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RingSizes {
    /// Indexed by [`TxQueue`] as `u8`; the beacon queue must be 1.
    pub tx: [u16; 8],
    pub rx: u16,
}

impl RingSizes {
    pub const DEFAULT: RingSizes = RingSizes {
        tx: [16, 64, 16, 16, 1, 32, 16, 32],
        rx: 128,
    };

    fn check(&self) -> Result<()> {
        let max = TRX_BD_IDX_MASK as u16;
        for q in TxQueue::ALL {
            let n = self.tx[q as usize];
            let ok = if q == TxQueue::Bcn {
                n == 1
            } else {
                (2..=max).contains(&n)
            };
            if !ok {
                return Err(Error::Invalid("pci: tx ring size"));
            }
        }
        if !(2..=max).contains(&self.rx) {
            return Err(Error::Invalid("pci: rx ring size"));
        }
        Ok(())
    }
}

fn round_up(n: usize) -> usize {
    n.div_ceil(PAGE) * PAGE
}

/// Bytes of one TX slot of `queue`.
pub fn tx_slot_size(queue: TxQueue) -> usize {
    if queue == TxQueue::Bcn {
        BCN_SLOT
    } else {
        TX_SLOT
    }
}

/// Offsets of every ring and slot array inside the three blocks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    pub sizes: RingSizes,
    /// Offsets into `desc`.
    pub tx_bd: [usize; 8],
    pub rx_bd: usize,
    pub desc_len: usize,
    /// Offsets into `txbuf`.
    pub tx_buf: [usize; 8],
    pub txbuf_len: usize,
    pub rxbuf_len: usize,
}

impl Layout {
    pub fn new(sizes: RingSizes) -> Result<Layout> {
        sizes.check()?;
        let mut tx_bd = [0; 8];
        let mut tx_buf = [0; 8];
        let (mut d, mut b) = (0, 0);
        for q in TxQueue::ALL {
            let n = usize::from(sizes.tx[q as usize]);
            tx_bd[q as usize] = d;
            d += round_up(n * TX_BUF_DESC_SZ);
            tx_buf[q as usize] = b;
            b += n * tx_slot_size(q);
        }
        let rx_bd = d;
        d += round_up(usize::from(sizes.rx) * RX_BUF_DESC_SZ);
        let layout = Layout {
            sizes,
            tx_bd,
            rx_bd,
            desc_len: d,
            tx_buf,
            txbuf_len: round_up(b),
            rxbuf_len: usize::from(sizes.rx) * RX_SLOT,
        };
        if [layout.desc_len, layout.txbuf_len, layout.rxbuf_len]
            .iter()
            .any(|&l| l > MAX_DMA_BLOCK)
        {
            return Err(Error::Invalid("pci: ring block over 4 MiB"));
        }
        Ok(layout)
    }

    /// Total DMA bytes the three blocks take.
    pub fn total(&self) -> usize {
        self.desc_len + self.txbuf_len + self.rxbuf_len
    }
}

/// The software side of one ring (upstream `struct rtw_pci_ring`): `wp` is
/// the next slot the host fills (TX) or the hardware index last seen (RX),
/// `rp` the next slot to reclaim (TX) or read (RX).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Ring {
    pub len: u32,
    pub wp: u32,
    pub rp: u32,
}

impl Ring {
    pub fn new(len: u16) -> Ring {
        Ring {
            len: u32::from(len),
            wp: 0,
            rp: 0,
        }
    }

    /// Free descriptors; one is kept empty so a full ring differs from an
    /// empty one (upstream `avail_desc`).
    pub fn avail(&self) -> u32 {
        avail_desc(self.wp, self.rp, self.len)
    }

    /// Descriptors the hardware has finished, from our `from` to its `to`
    /// (both already checked to be `< len`).
    pub fn distance(&self, from: u32, to: u32) -> u32 {
        if to >= from {
            to - from
        } else {
            self.len - (from - to)
        }
    }

    pub fn advance(&self, idx: u32) -> u32 {
        if idx + 1 >= self.len {
            0
        } else {
            idx + 1
        }
    }
}

/// Upstream `avail_desc`.
pub fn avail_desc(wp: u32, rp: u32, len: u32) -> u32 {
    if rp > wp {
        rp - wp - 1
    } else {
        len - wp + rp - 1
    }
}

/// One TX buffer descriptor (`struct rtw_pci_tx_buffer_desc` x 2, the
/// chip's 16-byte TX BD): segment 0 is the packet descriptor, segment 1 the
/// payload that follows it in the same slot (upstream `rtw_pci_tx_write_data`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TxBd {
    pub desc_len: u16,
    pub psb_len: u16,
    pub desc_dma: u32,
    pub payload_len: u16,
    pub payload_dma: u32,
}

impl TxBd {
    /// The BD for a packet of `total` bytes (descriptor included) copied to
    /// device address `dma`; `own` marks a beacon-queue BD.
    pub fn new(total: usize, dma: u32, own: bool) -> TxBd {
        let mut psb_len = ((total - 1) / 128 + 1) as u16;
        if own {
            psb_len |= 1 << super::regs::TXBD_OWN_OFFSET;
        }
        TxBd {
            desc_len: TX_PKT_DESC_SZ as u16,
            psb_len,
            desc_dma: dma,
            payload_len: (total - TX_PKT_DESC_SZ) as u16,
            payload_dma: dma + TX_PKT_DESC_SZ as u32,
        }
    }

    fn words(&self) -> [u32; 4] {
        [
            u32::from(self.desc_len) | u32::from(self.psb_len) << 16,
            self.desc_dma,
            u32::from(self.payload_len),
            self.payload_dma,
        ]
    }
}

/// Write `words` at `off` in `buf`, little-endian, one volatile store each.
fn store_words(buf: &DmaBuf, off: usize, words: &[u32]) {
    assert!(off.is_multiple_of(4) && off + words.len() * 4 <= buf.len);
    for (i, w) in words.iter().enumerate() {
        // SAFETY: `buf` is a live, page-aligned DMA mapping of `buf.len`
        // bytes owned by this driver (the `DmaAlloc` contract); the assert
        // keeps the aligned 4-byte store inside it.
        unsafe { ptr::write_volatile(buf.virt.add(off + i * 4).cast::<u32>(), w.to_le()) };
    }
}

/// Read `N` little-endian words at `off` in `buf`, one volatile load each.
fn load_words<const N: usize>(buf: &DmaBuf, off: usize) -> [u32; N] {
    assert!(off.is_multiple_of(4) && off + N * 4 <= buf.len);
    let mut out = [0u32; N];
    for (i, w) in out.iter_mut().enumerate() {
        // SAFETY: as in `store_words`, for an aligned 4-byte load.
        *w = u32::from_le(unsafe { ptr::read_volatile(buf.virt.add(off + i * 4).cast::<u32>()) });
    }
    out
}

/// Publish TX BD `bd` at `off` in the descriptor block.
pub fn write_tx_bd(desc: &DmaBuf, off: usize, bd: &TxBd) {
    store_words(desc, off, &bd.words());
}

/// The TX BD at `off`, as the device sees it (tests and diagnostics).
pub fn read_tx_bd(desc: &DmaBuf, off: usize) -> TxBd {
    let [w0, w1, w2, w3] = load_words::<4>(desc, off);
    TxBd {
        desc_len: w0 as u16,
        psb_len: (w0 >> 16) as u16,
        desc_dma: w1,
        payload_len: w2 as u16,
        payload_dma: w3,
    }
}

/// Zero the TX BD at `off`.
pub fn clear_tx_bd(desc: &DmaBuf, off: usize) {
    store_words(desc, off, &[0; 4]);
}

/// Arm the RX BD at `off` with the slot at device address `dma` (upstream
/// `rtw_pci_reset_rx_desc` / `rtw_pci_sync_rx_desc_device`: zeroed, then
/// `buf_size` and `dma`).
pub fn arm_rx_bd(desc: &DmaBuf, off: usize, dma: u32) {
    store_words(desc, off, &[RX_BUF_SIZE as u32, dma]);
}

/// The RX BD at `off`: `(buf_size, total_pkt_size, dma)`; the hardware
/// writes the RX tag into `total_pkt_size` when `BIT_RX_TAG_EN` is set.
pub fn read_rx_bd(desc: &DmaBuf, off: usize) -> (u16, u16, u32) {
    let [w0, w1] = load_words::<2>(desc, off);
    (w0 as u16, (w0 >> 16) as u16, w1)
}

/// Copy `src` into `buf` at `off` (a TX slot the device does not own).
pub fn copy_in(buf: &DmaBuf, off: usize, src: &[u8]) {
    assert!(off + src.len() <= buf.len);
    // SAFETY: the range is inside the live mapping (assert), the slot is not
    // in flight (the caller checked the ring has room), and `src` is a
    // separate Rust slice, so the regions do not overlap.
    unsafe { ptr::copy_nonoverlapping(src.as_ptr(), buf.virt.add(off), src.len()) };
}

/// Copy `dst.len()` bytes out of `buf` at `off` (an RX slot the hardware
/// handed back: it does not write it again until the host index passes it).
pub fn copy_out(buf: &DmaBuf, off: usize, dst: &mut [u8]) {
    assert!(off + dst.len() <= buf.len);
    // SAFETY: the range is inside the live mapping (assert) and `dst` is a
    // separate Rust slice, so the regions do not overlap.
    unsafe { ptr::copy_nonoverlapping(buf.virt.add(off), dst.as_mut_ptr(), dst.len()) };
}

/// Zero `len` bytes of `buf` from `off`.
pub fn zero(buf: &DmaBuf, off: usize, len: usize) {
    assert!(off + len <= buf.len);
    // SAFETY: the range is inside the live mapping (assert) and no device
    // DMA is running into it (called before the rings are handed over).
    unsafe { ptr::write_bytes(buf.virt.add(off), 0, len) };
}
