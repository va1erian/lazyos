//! Fuzzing the RX path with hostile bytes.
//!
//! [`run`] takes its input two ways. First as one received buffer for
//! [`query_rx_desc_with`]: it must never panic, and whatever it accepts must
//! lie inside the input and agree with the descriptor. Then as a script
//! against a small [`crate::pci::Pci`] whose fake device writes arbitrary
//! bytes into RX slots, arbitrary hardware indices and arbitrary RX BDs:
//! [`crate::pci::Pci::rx_poll`] must never panic, deliver only slices of a
//! slot, and leave every BD pointing at the driver's own slot.

use std::vec::Vec;

use super::*;
use crate::pci::fake::{FakeMmio, HeapDma};
use crate::pci::regs::RXBD_IDX_MPDUQ;
use crate::pci::ring::{RX_BUF_SIZE, RX_SLOT};
use crate::pci::{Pci, RingSizes};

/// The fields of an RX descriptor a test or a seed sets.
#[derive(Clone, Copy, Debug, Default)]
pub struct RxDescFields {
    pub pkt_len: u16,
    pub crc_err: bool,
    pub icv_err: bool,
    pub drv_info_units: u8,
    pub enc_type: u8,
    pub shift: u8,
    pub phy_status: bool,
    pub swdec: bool,
    pub cam_id: u8,
    pub c2h: bool,
    pub ppdu_cnt: u8,
    pub rate: u8,
    pub bw: u8,
    pub tsf_low: u32,
}

/// The 24-byte descriptor for `f`, by upstream's field masks.
pub fn encode_rx_desc(f: &RxDescFields) -> [u8; 24] {
    let put = |val: u32, mask: u32| (val << mask.trailing_zeros()) & mask;
    let w = [
        put(f.pkt_len.into(), W0_PKT_LEN)
            | put(f.crc_err.into(), W0_CRC32)
            | put(f.icv_err.into(), W0_ICV_ERR)
            | put(f.drv_info_units.into(), W0_DRV_INFO_SIZE)
            | put(f.enc_type.into(), W0_ENC_TYPE)
            | put(f.shift.into(), W0_SHIFT)
            | put(f.phy_status.into(), W0_PHYST)
            | put(f.swdec.into(), W0_SWDEC),
        put(f.cam_id.into(), W1_MACID),
        put(f.c2h.into(), W2_C2H) | put(f.ppdu_cnt.into(), W2_PPDU_CNT),
        put(f.rate.into(), W3_RX_RATE),
        put(f.bw.into(), W4_BW),
        f.tsf_low,
    ];
    let mut out = [0u8; 24];
    for (c, w) in out.as_chunks_mut::<4>().0.iter_mut().zip(w) {
        c.copy_from_slice(&w.to_le_bytes());
    }
    out
}

/// Check one buffer through the parser.
pub fn check_buffer(buf: &[u8]) {
    let mut stat = RxPktStat::default();
    let mut phy_len = None;
    let res = query_rx_desc_with(buf, &mut stat, |phy, _| phy_len = Some(phy.len()));
    let Ok(layout) = res else { return };
    assert!(layout.end() <= buf.len());
    assert_eq!(layout.len, usize::from(stat.pkt_len));
    assert_eq!(
        layout.offset,
        RX_PKT_DESC_SZ + usize::from(stat.shift) + usize::from(stat.drv_info_sz)
    );
    assert_eq!(rx_desc_extent(buf), Ok(layout.end()));
    assert_eq!(layout.payload(buf).len(), layout.len);
    assert!(stat.rate < DESC_RATE_MAX);
    if !stat.is_c2h {
        assert!(u32::from(stat.pkt_len) > FCS_LEN);
        assert_eq!(
            phy_len,
            stat.phy_status.then_some(usize::from(stat.drv_info_sz))
        );
    } else {
        assert_eq!(phy_len, None);
    }
}

struct Script<'a> {
    data: &'a [u8],
    at: usize,
}

impl Script<'_> {
    fn u8(&mut self) -> u8 {
        let b = self.data.get(self.at).copied().unwrap_or(0);
        self.at += 1;
        b
    }
    fn u16(&mut self) -> u16 {
        u16::from(self.u8()) | u16::from(self.u8()) << 8
    }
    fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| self.u8()).collect()
    }
    fn done(&self) -> bool {
        self.at >= self.data.len()
    }
}

/// Run `data` as a buffer, then as a hostile-device script.
pub fn run(data: &[u8]) {
    check_buffer(data);
    let mut s = Script { data, at: 0 };
    let rx = [2u16, 3, 4, 8][usize::from(s.u8() % 4)];
    let sizes = RingSizes {
        rx,
        ..RingSizes::DEFAULT
    };
    let mut pci = Pci::with_sizes(FakeMmio::default(), HeapDma::default(), sizes).unwrap();
    let rx_bd = pci.layout().rx_bd as u64;
    let desc_phys = pci.desc_phys();
    let slots = pci.rxbuf_phys();
    let mut scribbled = std::vec![false; usize::from(rx)];
    let mut steps = 0;
    while !s.done() && steps < 200 {
        steps += 1;
        match s.u8() % 4 {
            0 => {
                // A well-formed-ish descriptor with chosen lengths, then
                // junk, mostly in the next slots the driver will read.
                let ahead = u64::from(s.u8() % 3);
                let slot = (u64::from(pci.rx_rp()) + ahead) % u64::from(rx);
                let drv_info_units = [0, 4, 4, 4, 2, 15][usize::from(s.u8() % 6)];
                let f = RxDescFields {
                    pkt_len: if s.u8() & 1 != 0 {
                        s.u16() % 600
                    } else {
                        s.u16() % 12_000
                    },
                    drv_info_units,
                    shift: s.u8() % 4,
                    phy_status: drv_info_units != 0 && !s.u8().is_multiple_of(4),
                    c2h: s.u8() & 1 != 0,
                    rate: s.u8() % 0x60,
                    ..RxDescFields::default()
                };
                let mut buf = encode_rx_desc(&f).to_vec();
                let n = usize::from(s.u8());
                buf.extend(s.bytes(n));
                pci.dma().write(slots + slot * RX_SLOT as u64, &buf);
            }
            1 => {
                let slot = u64::from(s.u8() % rx as u8);
                let n = usize::from(s.u8());
                let junk = s.bytes(n);
                pci.dma().write(slots + slot * RX_SLOT as u64, &junk);
            }
            2 => {
                // Any hardware index, sometimes outside the ring.
                let raw = u32::from(s.u16());
                let idx = if raw & 7 == 0 {
                    raw >> 3 & 0xFFF
                } else {
                    (raw >> 3) % u32::from(rx)
                };
                pci.mmio().set32(RXBD_IDX_MPDUQ, idx << 16);
            }
            _ => {
                let slot = s.u8() % rx as u8;
                let junk = s.bytes(8);
                pci.dma()
                    .write(desc_phys + rx_bd + u64::from(slot) * 8, &junk);
                scribbled[usize::from(slot)] = true;
            }
        }
        if s.u8() & 1 != 0 {
            let before = pci.stats();
            let mut idx = pci.rx_rp();
            pci.rx_poll(u32::from(s.u8()), |p| {
                assert!(p.buf.len() <= RX_BUF_SIZE);
                assert!(p.layout.end() <= p.buf.len());
                assert_eq!(p.payload().len(), usize::from(p.stat.pkt_len));
            });
            let after = pci.stats();
            let processed =
                (after.rx_delivered + after.rx_dropped) - (before.rx_delivered + before.rx_dropped);
            for _ in 0..processed {
                scribbled[idx as usize] = false;
                idx = (idx + 1) % u32::from(rx);
            }
            assert_eq!(idx, pci.rx_rp());
            // Every BD not scribbled since it was last handed back points at
            // its own slot.
            for (i, dirty) in scribbled.iter().enumerate() {
                if *dirty {
                    continue;
                }
                let bd = pci.dma().read(desc_phys + rx_bd + i as u64 * 8, 8);
                let size = u16::from_le_bytes([bd[0], bd[1]]);
                let dma = u32::from_le_bytes([bd[4], bd[5], bd[6], bd[7]]);
                assert_eq!(usize::from(size), RX_BUF_SIZE);
                assert_eq!(u64::from(dma), slots + i as u64 * RX_SLOT as u64);
            }
        }
    }
}
