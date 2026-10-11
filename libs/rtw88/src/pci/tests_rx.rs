//! The RX ring against the fakes.

use std::vec::Vec;

use super::regs::*;
use super::ring::{RingSizes, RX_BUF_SIZE, RX_SLOT};
use super::tests::{pci_with, TestPci};
use crate::hci::Hci;
use crate::rx::fuzz::{encode_rx_desc, RxDescFields};

fn ring4() -> RingSizes {
    RingSizes {
        rx: 4,
        ..RingSizes::DEFAULT
    }
}

/// A received 802.11 frame of `len` bytes (FCS included) with a PHY status.
fn frame_buf(len: u16, fill: u8) -> Vec<u8> {
    let f = RxDescFields {
        pkt_len: len,
        drv_info_units: 4,
        phy_status: true,
        shift: 2,
        rate: 0x04,
        ..RxDescFields::default()
    };
    let mut buf = encode_rx_desc(&f).to_vec();
    buf.extend(std::iter::repeat_n(0xEE, 2 + 32));
    buf.extend(std::iter::repeat_n(fill, usize::from(len)));
    buf
}

/// Put `buf` in slot `idx` and the hardware write index at `wp`.
fn deliver(pci: &TestPci, idx: u32, buf: &[u8], wp: u32) {
    pci.dma()
        .write(pci.rxbuf_phys() + u64::from(idx) * RX_SLOT as u64, buf);
    // The hardware stamps the BD with the running RX tag.
    let off = pci.layout().rx_bd as u64 + u64::from(idx) * 8 + 2;
    let ahead = (idx + pci.rx.len - pci.rx.rp) % pci.rx.len;
    let tag = (pci.rx_tag + ahead as u16) % 8192;
    pci.dma().write(pci.desc_phys() + off, &tag.to_le_bytes());
    pci.mmio().set32(RXBD_IDX_MPDUQ, wp << 16);
}

fn poll_all(pci: &mut TestPci) -> Vec<(bool, Vec<u8>)> {
    let mut got = Vec::new();
    pci.rx_poll(64, |p| got.push((p.stat.is_c2h, p.payload().to_vec())));
    got
}

#[test]
fn rx_poll_delivers_rearms_and_moves_the_host_index() {
    let mut pci = pci_with(ring4());
    pci.setup().unwrap();
    pci.start().unwrap();
    deliver(&pci, 0, &frame_buf(40, 0x11), 1);
    // Scribble on the BD as a device would: the poll re-arms it anyway.
    pci.dma()
        .write(pci.desc_phys() + pci.layout().rx_bd as u64, &[0xFF; 8]);
    let got = poll_all(&mut pci);
    assert_eq!(got, std::vec![(false, std::vec![0x11; 40])]);
    assert_eq!(pci.mmio().last_write(2, RXBD_IDX_MPDUQ), Some(1));
    let bd = pci
        .dma()
        .read(pci.desc_phys() + pci.layout().rx_bd as u64, 8);
    assert_eq!(&bd[..4], &(RX_BUF_SIZE as u32).to_le_bytes());
    assert_eq!(&bd[4..], &(pci.rxbuf_phys() as u32).to_le_bytes());
    // Nothing new: nothing delivered, no index write.
    pci.mmio().clear_log();
    assert!(poll_all(&mut pci).is_empty());
    assert_eq!(pci.mmio().last_write(2, RXBD_IDX_MPDUQ), None);
}

#[test]
fn rx_wraps_and_hands_c2h_over() {
    let mut pci = pci_with(ring4());
    pci.setup().unwrap();
    let mut seen = Vec::new();
    for n in 0..10u32 {
        let idx = n % 4;
        let buf = if n % 3 == 0 {
            let f = RxDescFields {
                pkt_len: 12,
                c2h: true,
                ..RxDescFields::default()
            };
            let mut b = encode_rx_desc(&f).to_vec();
            b.extend([n as u8; 12]);
            b
        } else {
            frame_buf(20, n as u8)
        };
        deliver(&pci, idx, &buf, (n + 1) % 4);
        seen.extend(poll_all(&mut pci));
        assert_eq!(pci.mmio().last_write(2, RXBD_IDX_MPDUQ), Some((n + 1) % 4));
    }
    assert_eq!(seen.len(), 10);
    for (n, (c2h, payload)) in seen.iter().enumerate() {
        assert_eq!(*c2h, n % 3 == 0);
        assert!(payload.iter().all(|b| *b == n as u8));
    }
    assert_eq!(pci.stats().rx_tag_mismatch, 0);
}

#[test]
fn several_buffers_in_one_poll_and_the_budget() {
    let mut pci = pci_with(ring4());
    pci.setup().unwrap();
    pci.start().unwrap();
    for i in 0..3 {
        deliver(&pci, i, &frame_buf(30, i as u8), 3);
    }
    let mask0 = pci.mmio().last_write(4, HIMR0).unwrap();
    pci.mmio().clear_log();
    // A budget of two leaves one, and RX masked as upstream's NAPI does.
    let mut got = 0;
    assert_eq!(pci.rx_poll(2, |_| got += 1), 2);
    assert_eq!(got, 2);
    assert_eq!(pci.mmio().last_write(4, HIMR0), None);
    assert_eq!(pci.rx_pending(), 1);
    assert_eq!(pci.rx_poll(2, |_| got += 1), 1);
    assert_eq!(pci.mmio().last_write(4, HIMR0), Some(mask0));
}

#[test]
fn bad_buffers_are_dropped_and_their_slots_reused() {
    let mut pci = pci_with(ring4());
    pci.setup().unwrap();
    // A length past the slot, a bad driver-info size, an FCS-only frame,
    // then a good one.
    let mut too_long = frame_buf(30, 0);
    too_long[0] = 0xFF;
    too_long[1] = 0x3F; // pkt_len 16383
    let mut bad_info = frame_buf(30, 1);
    bad_info[2] = (bad_info[2] & 0xF0) | 3;
    let short = frame_buf(4, 2);
    deliver(&pci, 0, &too_long, 1);
    deliver(&pci, 1, &bad_info, 2);
    deliver(&pci, 2, &short, 3);
    assert!(poll_all(&mut pci).is_empty());
    // The hardware fills at most len - 1 before the host reads.
    deliver(&pci, 3, &frame_buf(30, 3), 0);
    let got = poll_all(&mut pci);
    assert_eq!(got, std::vec![(false, std::vec![3; 30])]);
    assert_eq!(pci.stats().rx_dropped, 3);
    // What fits RTK_PCI_RX_BUF_SIZE behind 58 bytes of descriptor, PHY
    // status and shift is delivered; one byte more is dropped, as upstream
    // drops an "oversized RX packet".
    let fits = (RX_BUF_SIZE - 58) as u16;
    deliver(&pci, 0, &frame_buf(fits + 1, 0), 1);
    deliver(&pci, 1, &frame_buf(fits, 0), 2);
    let got = poll_all(&mut pci);
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].1.len(), usize::from(fits));
    assert_eq!(pci.stats().rx_dropped, 4);
}

#[test]
fn a_lying_write_index_and_a_tag_gap_are_counted() {
    let mut pci = pci_with(ring4());
    pci.setup().unwrap();
    pci.mmio().set32(RXBD_IDX_MPDUQ, 9 << 16);
    assert!(poll_all(&mut pci).is_empty());
    assert_eq!(pci.stats().rx_bad_hw_index, 1);
    // A buffer whose BD carries the wrong tag is still delivered.
    deliver(&pci, 0, &frame_buf(30, 7), 1);
    let off = pci.layout().rx_bd as u64 + 2;
    pci.dma().write(pci.desc_phys() + off, &[0x34, 0x12]);
    assert_eq!(poll_all(&mut pci).len(), 1);
    assert_eq!(pci.stats().rx_tag_mismatch, 1);
}
