//! Ring set-up, TX, interrupts and the PCIe PHY against the fakes.

use std::vec::Vec;

use super::fake::{FakeMmio, HeapDma};
use super::regs::*;
use super::ring::{self, RingSizes, BCN_SLOT, RX_BUF_SIZE, RX_SLOT, TX_SLOT};
use super::{Pci, PciStats};
use crate::error::Error;
use crate::hci::{Hci, TxQueue};
use crate::tx::{self, TxPktInfo, TX_PKT_DESC_SZ};

pub(super) type TestPci = Pci<FakeMmio, HeapDma>;

pub(super) fn pci_with(sizes: RingSizes) -> TestPci {
    Pci::with_sizes(FakeMmio::default(), HeapDma::default(), sizes).unwrap()
}

fn small() -> RingSizes {
    RingSizes {
        tx: [4, 4, 4, 4, 1, 4, 4, 4],
        rx: 4,
    }
}

/// A packet: a descriptor for `len` bytes of payload, then the payload.
fn pkt(len: usize, fill: u8) -> Vec<u8> {
    let info = TxPktInfo {
        qsel: tx::QSEL_MGMT,
        ..TxPktInfo::default()
    };
    tx::build_pkt(&info, &std::vec![fill; len])
}

/// Make the hardware's read pointer of `queue` `rp`.
fn hw_read(pci: &TestPci, queue: TxQueue, rp: u32) {
    let addr = super::txq::idx_addr(queue).unwrap();
    pci.mmio().set32(addr, rp << 16);
}

#[test]
fn default_budget_fits_three_blocks() {
    let layout = ring::Layout::new(RingSizes::DEFAULT).unwrap();
    assert_eq!(layout.desc_len, 9 * 4096);
    assert_eq!(layout.txbuf_len, 192 * TX_SLOT + BCN_SLOT);
    assert_eq!(layout.rxbuf_len, 128 * RX_SLOT);
    assert!(layout.total() < 2 << 20, "{}", layout.total());
    let pci = TestPci::new(FakeMmio::default(), HeapDma::default()).unwrap();
    assert_eq!(pci.layout(), &layout);
}

#[test]
fn ring_sizes_and_blocks_are_checked() {
    let mut bad = RingSizes::DEFAULT;
    bad.tx[TxQueue::Bcn as usize] = 2;
    assert!(ring::Layout::new(bad).is_err());
    bad = RingSizes::DEFAULT;
    bad.tx[TxQueue::Be as usize] = 1;
    assert!(ring::Layout::new(bad).is_err());
    bad = RingSizes {
        rx: 4096,
        ..RingSizes::DEFAULT
    };
    assert!(ring::Layout::new(bad).is_err());
    // 400 RX slots of 12 KiB do not fit one 4 MiB block.
    bad = RingSizes {
        rx: 400,
        ..RingSizes::DEFAULT
    };
    assert!(ring::Layout::new(bad).is_err());

    let mut high = HeapDma::default();
    high.next_phys = 0xFFFF_0000;
    assert!(Pci::with_sizes(FakeMmio::default(), high, small()).is_err());
    let mut skewed = HeapDma::default();
    skewed.misalign = true;
    assert!(Pci::with_sizes(FakeMmio::default(), skewed, small()).is_err());
    let mut short = HeapDma::default();
    short.limit = 2;
    assert!(matches!(
        Pci::with_sizes(FakeMmio::default(), short, small()),
        Err(Error::NoMemory)
    ));
}

#[test]
fn setup_programs_every_ring_like_reset_buf_desc() {
    let mut pci = TestPci::new(FakeMmio::default(), HeapDma::default()).unwrap();
    pci.mmio().set32(PCI_CTRL, 0x0800_0000);
    pci.setup().unwrap();
    let m = pci.mmio();
    let desc = pci.desc_phys();
    let l = *pci.layout();
    let bd = |q: TxQueue| (desc + l.tx_bd[q as usize] as u64) as u32;
    let expected: Vec<(u8, u32, u32)> = std::vec![
        (1, PCI_CTRL + 3, 0x08 | 0xf7),
        (4, TXBD_DESA_BCNQ, bd(TxQueue::Bcn)),
        (2, TXBD_NUM_H2CQ, 32),
        (4, TXBD_DESA_H2CQ, bd(TxQueue::H2c)),
        (2, TXBD_NUM_BKQ, 16),
        (4, TXBD_DESA_BKQ, bd(TxQueue::Bk)),
        (2, TXBD_NUM_BEQ, 64),
        (4, TXBD_DESA_BEQ, bd(TxQueue::Be)),
        (2, TXBD_NUM_VOQ, 16),
        (4, TXBD_DESA_VOQ, bd(TxQueue::Vo)),
        (2, TXBD_NUM_VIQ, 16),
        (4, TXBD_DESA_VIQ, bd(TxQueue::Vi)),
        (2, TXBD_NUM_MGMTQ, 32),
        (4, TXBD_DESA_MGMTQ, bd(TxQueue::Mgmt)),
        (2, TXBD_NUM_HI0Q, 16),
        (4, TXBD_DESA_HI0Q, bd(TxQueue::Hi0)),
        (2, RXBD_NUM_MPDUQ, 128),
        (4, RXBD_DESA_MPDUQ, (desc + l.rx_bd as u64) as u32),
        (4, TXBD_RWPTR_CLR, 0xffff_ffff),
        (
            4,
            TXBD_H2CQ_CSR,
            BIT_CLR_H2CQ_HOST_IDX | BIT_CLR_H2CQ_HW_IDX
        ),
        // rtw_pci_dma_reset
        (
            4,
            PCI_CTRL,
            0xff00_0000 | BIT_RST_TRXDMA_INTF | BIT_RX_TAG_EN
        ),
    ];
    assert_eq!(m.writes(), expected);
    // Every RX BD is armed with its own slot.
    let rx0 = pci.dma().read(desc + l.rx_bd as u64, 16);
    let slot = pci.rxbuf_phys();
    assert_eq!(&rx0[..2], &(RX_BUF_SIZE as u16).to_le_bytes());
    assert_eq!(&rx0[4..8], &(slot as u32).to_le_bytes());
    assert_eq!(
        &rx0[12..16],
        &((slot + RX_SLOT as u64) as u32).to_le_bytes()
    );
}

#[test]
fn tx_write_fills_the_bd_and_kick_moves_the_host_index() {
    let mut pci = pci_with(small());
    pci.setup().unwrap();
    let p = pkt(100, 0xAB);
    pci.tx_write(TxQueue::Mgmt, &p).unwrap();
    // Nothing reaches the hardware before the kick.
    assert_eq!(pci.mmio().last_write(2, TXBD_IDX_MGMTQ), None);
    let bd = pci.tx_bd(TxQueue::Mgmt, 0).unwrap();
    let slot = pci.txbuf_phys() + pci.layout().tx_buf[TxQueue::Mgmt as usize] as u64;
    assert_eq!(bd.desc_len, 48);
    assert_eq!(bd.psb_len, 2); // 148 bytes in 128-byte units, rounded up
    assert_eq!(u64::from(bd.desc_dma), slot);
    assert_eq!(bd.payload_len, 100);
    assert_eq!(u64::from(bd.payload_dma), slot + 48);
    assert_eq!(pci.dma().read(slot, p.len()), p);

    pci.tx_write(TxQueue::Be, &pkt(10, 1)).unwrap();
    pci.tx_kick_off();
    assert_eq!(pci.mmio().last_write(2, TXBD_IDX_MGMTQ), Some(1));
    assert_eq!(pci.mmio().last_write(2, TXBD_IDX_BEQ), Some(1));
    assert_eq!(pci.mmio().last_write(2, TXBD_IDX_BKQ), None);
    // A second kick with nothing new writes nothing.
    pci.mmio().clear_log();
    pci.tx_kick_off();
    assert!(pci.mmio().writes().is_empty());
}

#[test]
fn psb_len_counts_128_byte_units() {
    assert_eq!(ring::TxBd::new(128, 0, false).psb_len, 1);
    assert_eq!(ring::TxBd::new(129, 0, false).psb_len, 2);
    assert_eq!(ring::TxBd::new(49, 0, true).psb_len, 1 | 1 << 15);
}

#[test]
fn full_ring_refuses_and_reclaim_frees_it() {
    let mut pci = pci_with(small());
    pci.setup().unwrap();
    for i in 0..3 {
        pci.tx_write(TxQueue::Vo, &pkt(20, i)).unwrap();
    }
    // One descriptor stays empty: 4 entries hold 3 packets.
    assert_eq!(pci.tx_avail(TxQueue::Vo), 0);
    assert_eq!(
        pci.tx_write(TxQueue::Vo, &pkt(20, 9)),
        Err(Error::Full("pci: tx ring"))
    );
    pci.tx_kick_off();
    // The hardware sent two: the next write reclaims them by itself.
    hw_read(&pci, TxQueue::Vo, 2);
    pci.tx_write(TxQueue::Vo, &pkt(20, 3)).unwrap();
    assert_eq!(pci.tx_avail(TxQueue::Vo), 1);
    assert_eq!(pci.stats().tx_reclaimed, 2);
}

#[test]
fn indices_wrap_around_the_ring() {
    let mut pci = pci_with(small());
    pci.setup().unwrap();
    let mut sent = 0u32;
    for round in 0..10u32 {
        for _ in 0..2 {
            pci.tx_write(TxQueue::Bk, &pkt(30, round as u8)).unwrap();
        }
        pci.tx_kick_off();
        sent += 2;
        let wp = sent % 4;
        assert_eq!(pci.mmio().last_write(2, TXBD_IDX_BKQ), Some(wp));
        hw_read(&pci, TxQueue::Bk, wp);
        assert_eq!(pci.tx_reclaim(TxQueue::Bk), 2);
        // The slot written last holds this round's packet.
        let last = (sent - 1) % 4;
        let bd = pci.tx_bd(TxQueue::Bk, last).unwrap();
        let byte = pci.dma().read(u64::from(bd.payload_dma), 1)[0];
        assert_eq!(byte, round as u8);
    }
    assert_eq!(pci.tx_avail(TxQueue::Bk), 3);
}

#[test]
fn a_lying_read_pointer_is_ignored() {
    let mut pci = pci_with(small());
    pci.setup().unwrap();
    pci.tx_write(TxQueue::Be, &pkt(20, 0)).unwrap();
    pci.tx_kick_off();
    // Past the write index, then outside the ring, then all ones.
    for rp in [2, 7, 0xFFF] {
        hw_read(&pci, TxQueue::Be, rp);
        assert_eq!(pci.tx_reclaim(TxQueue::Be), 0);
    }
    pci.mmio().set32(TXBD_IDX_BEQ, 0xFFFF_FFFF);
    assert_eq!(pci.tx_reclaim(TxQueue::Be), 0);
    assert_eq!(pci.stats().tx_bad_hw_index, 4);
    assert_eq!(pci.tx_avail(TxQueue::Be), 2);
}

#[test]
fn oversized_and_empty_packets_are_refused() {
    let mut pci = pci_with(small());
    pci.setup().unwrap();
    assert!(pci.tx_write(TxQueue::Be, &[0; TX_PKT_DESC_SZ]).is_err());
    assert!(pci.tx_write(TxQueue::Be, &pkt(TX_SLOT, 0)).is_err());
    assert!(pci
        .tx_write(TxQueue::Be, &pkt(TX_SLOT - TX_PKT_DESC_SZ, 0))
        .is_ok());
    assert!(pci.write_data_rsvd_page(&pkt(BCN_SLOT, 0)).is_err());
}

#[test]
fn rsvd_page_goes_through_the_beacon_queue_and_waits_for_valid() {
    let mut pci = pci_with(small());
    pci.setup().unwrap();
    let page = tx::rsvd_page_pkt(&[0x5A; 4096], crate::dev::Band::Ghz2);
    // The hardware never says the page landed: a timeout.
    pci.mmio().clear_log();
    let err = pci.write_data_rsvd_page(&page).unwrap_err();
    assert_eq!(
        err,
        Error::Timeout {
            step: "rsvd page",
            addr: REG_FIFOPAGE_CTRL_2
        }
    );
    assert_eq!(
        pci.mmio().last_write(1, TXBD_BCN_WORK),
        Some(u32::from(BIT_PCI_BCNQ_FLAG))
    );
    assert!(pci.mmio().delayed_us() >= 50_000);

    // It does: done at once, on the same single slot, OWN set.
    pci.mmio()
        .set32(REG_FIFOPAGE_CTRL_2, u32::from(BIT_BCN_VALID_V1));
    pci.write_data_rsvd_page(&page).unwrap();
    let bd = pci.tx_bd(TxQueue::Bcn, 0).unwrap();
    assert_eq!(bd.psb_len, (4144u16.div_ceil(128)) | 1 << 15);
    assert_eq!(bd.payload_len, 4096);
    assert_eq!(pci.dma().read(u64::from(bd.desc_dma), page.len()), page);
    // The beacon queue never has a host index to kick.
    pci.tx_kick_off();
    assert_eq!(pci.tx_avail(TxQueue::Bcn), 0);
}

#[test]
fn h2c_is_kicked_at_once() {
    let mut pci = pci_with(small());
    pci.setup().unwrap();
    let h2c = tx::h2c_pkt(&[1, 2, 3, 4, 5, 6, 7, 8]);
    pci.write_data_h2c(&h2c).unwrap();
    assert_eq!(pci.mmio().last_write(2, TXBD_IDX_H2CQ), Some(1));
    let bd = pci.tx_bd(TxQueue::H2c, 0).unwrap();
    assert_eq!(bd.payload_len, 8);
}

#[test]
fn interrupt_masks_reads_acks_reclaims_and_unmasks() {
    let mut pci = pci_with(small());
    pci.setup().unwrap();
    pci.start().unwrap();
    let mask0 = pci.mmio().last_write(4, HIMR0).unwrap();
    assert_eq!(
        mask0 & (IMR_ROK | IMR_MGNTDOK | IMR_C2HCMD),
        IMR_ROK | IMR_MGNTDOK | IMR_C2HCMD
    );
    assert_eq!(pci.mmio().last_write(4, HIMR1), Some(IMR_TXFOVW));
    assert_eq!(pci.mmio().last_write(4, HIMR3), Some(IMR_H2CDOK));

    pci.tx_write(TxQueue::Mgmt, &pkt(10, 0)).unwrap();
    pci.write_data_h2c(&tx::h2c_pkt(&[0; 8])).unwrap();
    pci.tx_kick_off();
    hw_read(&pci, TxQueue::Mgmt, 1);
    hw_read(&pci, TxQueue::H2c, 1);
    // Causes the driver does not unmask (TBDOK) are left alone.
    pci.mmio().set32(HISR0, IMR_ROK | IMR_MGNTDOK | IMR_TBDOK);
    pci.mmio().set32(HISR1, IMR_TXFOVW | IMR_RXERR);
    pci.mmio().set32(HISR3, IMR_H2CDOK);
    pci.mmio().clear_log();
    let ev = pci.interrupt();
    assert!(ev.rx && !ev.c2h && ev.tx_fifo_overflow && !ev.is_empty());
    assert_eq!(ev.tx_done[TxQueue::Mgmt as usize], 1);
    assert_eq!(ev.tx_done[TxQueue::H2c as usize], 1);
    assert_eq!(
        ev.status,
        [IMR_ROK | IMR_MGNTDOK, IMR_TXFOVW, 0, IMR_H2CDOK]
    );
    let w = pci.mmio().writes();
    // Masked first, causes cleared, unmasked last without ROK.
    assert_eq!(&w[..3], &[(4, HIMR0, 0), (4, HIMR1, 0), (4, HIMR3, 0)]);
    assert_eq!(
        &w[3..6],
        &[
            (4, HISR0, IMR_ROK | IMR_MGNTDOK),
            (4, HISR1, IMR_TXFOVW),
            (4, HISR3, IMR_H2CDOK)
        ]
    );
    assert_eq!(w[6], (4, HIMR0, mask0 & !IMR_ROK));
    // rx_poll with nothing pending unmasks RX again.
    pci.rx_poll(64, |_| panic!("nothing to deliver"));
    assert_eq!(pci.mmio().last_write(4, HIMR0), Some(mask0));
}

#[test]
fn a_spurious_interrupt_is_empty_and_stop_masks_for_good() {
    let mut pci = pci_with(small());
    pci.setup().unwrap();
    pci.start().unwrap();
    pci.mmio().set32(HISR0, IMR_TBDER);
    assert!(pci.interrupt().is_empty());
    assert!(pci.irq_enabled());
    pci.stop();
    assert!(!pci.irq_enabled());
    assert_eq!(pci.mmio().last_write(4, HIMR0), Some(0));
    pci.mmio().set32(HISR0, IMR_C2HCMD);
    assert!(pci.interrupt().c2h);
    assert!(!pci.irq_enabled());
    assert_eq!(pci.stats(), PciStats::default());
}

#[test]
fn phy_cfg_turns_aspm_and_clkreq_off_over_dbi() {
    let mut pci = pci_with(small());
    pci.mmio()
        .set_dbi(RTK_PCIE_LINK_CFG, BIT_L1_SW_EN | BIT_CLKREQ_SW_EN | 0x40);
    pci.mmio().set_dbi(RTK_PCIE_CLKDLY_CTRL, 0x33);
    pci.phy_cfg(3, 0).unwrap();
    assert_eq!(pci.mmio().dbi(RTK_PCIE_LINK_CFG), 0x40);
    assert_eq!(pci.mmio().dbi(RTK_PCIE_CLKDLY_CTRL), 0);
    // The 8822C's PHY tables are empty: no MDIO write.
    assert!(pci.mmio().mdio().is_empty());
    // DBI write of 0x0725 (lane 1 of dword 0x724): byte enable bit 13.
    let w = pci.mmio().writes();
    assert!(w.contains(&(1, REG_DBI_WDATA_V1 + 1, 0)));
    assert!(w.contains(&(2, REG_DBI_FLAG_V1, 0x0724 | 1 << 13)));
    // RFE 5 sets CF_L.
    pci.phy_cfg(3, 5).unwrap();
    assert_eq!(pci.mmio().peek32(REG_ANAPARSW_MAC_0) & BIT_CF_L_V2, 1 << 28);
}

#[test]
fn interface_cfg_needs_cut_d() {
    for (cut, set) in [(2u8, false), (3, true)] {
        let mut pci = pci_with(small());
        pci.interface_cfg(); // before phy_cfg: the cut is unknown, nothing
        assert_eq!(pci.mmio().peek32(REG_HCI_MIX_CFG), 0);
        pci.phy_cfg(cut, 0).unwrap();
        pci.interface_cfg();
        let v = pci.mmio().peek32(REG_HCI_MIX_CFG) & BIT_PCIE_EMAC_PDN_AUX_TO_FAST_CLK;
        assert_eq!(v != 0, set);
    }
}

#[test]
fn stuck_dbi_and_mdio_time_out() {
    let mut pci = pci_with(small());
    pci.mmio().set_dbi(0x10, 0x77);
    assert_eq!(pci.dbi_read8(0x10), Ok(0x77));
    pci.mdio_write(0x21, 0xBEEF, false).unwrap();
    assert_eq!(pci.mmio().mdio(), std::vec![(3, 0x01, 0xBEEF)]);
    let mut stuck = pci_with(small());
    stuck.mmio.stuck = true;
    assert!(matches!(
        stuck.dbi_write8(0x719, 0),
        Err(Error::Timeout { .. })
    ));
    assert!(matches!(
        stuck.mdio_write(1, 0, true),
        Err(Error::Timeout { .. })
    ));
    // phy_cfg still runs every step and reports the first failure.
    assert!(stuck.phy_cfg(3, 5).is_err());
    assert_ne!(stuck.mmio().peek32(REG_ANAPARSW_MAC_0), 0);
}
