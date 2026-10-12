//! The PCIe PHY and link (upstream `rtw_dbi_write8`, `rtw_dbi_read8`,
//! `rtw_mdio_write`, `rtw_pci_phy_cfg`, `rtw_pci_link_cfg`,
//! `rtw_pci_aspm_set`, `rtw_pci_clkreq_set`, and the 8822C's
//! `phy_para_table_8822c` from `rtw8822c.c`).
//!
//! **ASPM and CLKREQ.** The chip implements both in a module the driver
//! switches on (`BIT_L1_SW_EN`, `BIT_CLKREQ_SW_EN` in DBI `0x719`), next to
//! the standard link control register. Upstream enables CLKREQ when the host
//! did and ASPM L1 only in power save. This driver never enters power save
//! and cannot write PCI configuration space, so it switches both chip-side
//! modules *off* explicitly: the reference clock stays requested and the
//! chip never initiates L1, whatever the firmware left in link control. The
//! chip has no L0s switch here; upstream never touches L0s either.

use crate::error::{Error, Result};
use crate::hci::RegExt;

use super::regs::*;
use super::{DmaAlloc, Mmio, Pci};

/// Which block an interface parameter goes to (upstream `enum rtw_ip_sel`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IpSel {
    /// The PCIe PHY, over MDIO.
    Phy,
    /// The MAC side, over DBI.
    Mac,
    /// The PCIe core, over DBI.
    Dbi,
}

/// One PCIe PHY parameter (upstream `struct rtw_intf_phy_para`); an
/// `offset` of `0xffff` ends a table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IntfPhyPara {
    pub offset: u16,
    pub value: u16,
    pub ip_sel: IpSel,
    /// Cuts it applies to, bit `n` for cut `n` (`0xffff`: all).
    pub cut_mask: u16,
    pub platform: u16,
}

const END_8822C: IntfPhyPara = IntfPhyPara {
    offset: 0xFFFF,
    value: 0x0000,
    ip_sel: IpSel::Phy,
    cut_mask: 0xFFFF,
    platform: 0,
};

/// `pcie_gen1_param_8822c`: the 8822C needs no gen 1 PHY parameter.
pub const PCIE_GEN1_PARA_8822C: &[IntfPhyPara] = &[END_8822C];
/// `pcie_gen2_param_8822c`: nor a gen 2 one.
pub const PCIE_GEN2_PARA_8822C: &[IntfPhyPara] = &[END_8822C];

/// Keep the first error of a sequence that, like upstream, carries on.
fn first(acc: &mut Result<()>, r: Result<()>) {
    if acc.is_ok() {
        *acc = r;
    }
}

impl<M: Mmio, D: DmaAlloc> Pci<M, D> {
    /// Poll a DBI/MDIO busy flag until it reads clear, 10 us apart,
    /// [`RTW_PCI_WR_RETRY_CNT`] times; `width` is the access upstream uses.
    fn wait_flag_clear(
        &mut self,
        step: &'static str,
        addr: u32,
        width: u8,
        mask: u32,
    ) -> Result<()> {
        for _ in 0..RTW_PCI_WR_RETRY_CNT {
            let v = match width {
                1 => u32::from(self.mmio.read8(addr)),
                _ => self.mmio.read32(addr),
            };
            if v & mask == 0 {
                return Ok(());
            }
            self.mmio.delay_us(10);
        }
        Err(Error::Timeout { step, addr })
    }

    /// Upstream `rtw_dbi_write8`.
    pub fn dbi_write8(&mut self, addr: u16, data: u8) -> Result<()> {
        let remainder = addr & !(BITS_DBI_WREN | BITS_DBI_ADDR_MASK) & 0x3;
        let write_addr = (addr & BITS_DBI_ADDR_MASK) | ((1u16 << remainder) << 12 & BITS_DBI_WREN);
        self.mmio
            .write8(REG_DBI_WDATA_V1 + u32::from(remainder), data);
        self.mmio.write16(REG_DBI_FLAG_V1, write_addr);
        self.mmio
            .write8(REG_DBI_FLAG_V1 + 2, (BIT_DBI_WFLAG >> 16) as u8);
        self.wait_flag_clear("dbi write", REG_DBI_FLAG_V1 + 2, 1, 0xFF)
    }

    /// Upstream `rtw_dbi_read8`.
    pub fn dbi_read8(&mut self, addr: u16) -> Result<u8> {
        self.mmio
            .write16(REG_DBI_FLAG_V1, addr & BITS_DBI_ADDR_MASK);
        self.mmio
            .write8(REG_DBI_FLAG_V1 + 2, (BIT_DBI_RFLAG >> 16) as u8);
        self.wait_flag_clear("dbi read", REG_DBI_FLAG_V1 + 2, 1, 0xFF)?;
        Ok(self.mmio.read8(REG_DBI_RDATA_V1 + u32::from(addr & 3)))
    }

    /// Upstream `rtw_mdio_write`: `g1` selects the gen 1 page set.
    pub fn mdio_write(&mut self, addr: u8, data: u16, g1: bool) -> Result<()> {
        self.mmio.write16(REG_MDIO_V1, data);
        let mut page = if addr < RTW_PCI_MDIO_PG_SZ { 0 } else { 1 };
        page += if g1 {
            RTW_PCI_MDIO_PG_OFFS_G1
        } else {
            RTW_PCI_MDIO_PG_OFFS_G2
        };
        self.mmio
            .write8(REG_PCIE_MIX_CFG, addr & BITS_MDIO_ADDR_MASK);
        self.mmio.write8(REG_PCIE_MIX_CFG + 3, page);
        self.write32_mask(REG_PCIE_MIX_CFG, BIT_MDIO_WFLAG_V1, 1);
        self.wait_flag_clear("mdio write", REG_PCIE_MIX_CFG, 4, BIT_MDIO_WFLAG_V1)
    }

    /// Set or clear `bits` of the chip's link configuration over DBI
    /// (upstream `rtw_pci_aspm_set` / `rtw_pci_clkreq_set`).
    fn link_cfg_bits(&mut self, bits: u8, enable: bool) -> Result<()> {
        let value = self.dbi_read8(RTK_PCIE_LINK_CFG)?;
        let value = if enable { value | bits } else { value & !bits };
        self.dbi_write8(RTK_PCIE_LINK_CFG, value)
    }

    /// Upstream `rtw_pci_aspm_set`: the chip-side ASPM L1 module.
    pub fn aspm_set(&mut self, enable: bool) -> Result<()> {
        self.link_cfg_bits(BIT_L1_SW_EN, enable)
    }

    /// Upstream `rtw_pci_clkreq_set`: the chip-side CLKREQ# module.
    pub fn clkreq_set(&mut self, enable: bool) -> Result<()> {
        self.link_cfg_bits(BIT_CLKREQ_SW_EN, enable)
    }

    fn apply_para(&mut self, table: &[IntfPhyPara], cut: u16, g1: bool) -> Result<()> {
        let mut acc = Ok(());
        for para in table {
            if para.cut_mask & cut == 0 {
                continue;
            }
            if para.offset == 0xffff {
                break;
            }
            let r = match para.ip_sel {
                IpSel::Phy => self.mdio_write(para.offset as u8, para.value, g1),
                _ => self.dbi_write8(para.offset, para.value as u8),
            };
            first(&mut acc, r);
        }
        acc
    }

    /// Upstream `rtw_pci_phy_cfg` with `rtw_pci_link_cfg`, for an 8822C of
    /// cut `cut_version` (`hal.cut_version`) and RFE option `rfe_option`
    /// (`efuse.rfe_option`). Every step runs even if one fails (upstream
    /// warns and carries on); the first failure is returned.
    pub fn phy_cfg(&mut self, cut_version: u8, rfe_option: u8) -> Result<()> {
        self.cut_version = Some(cut_version);
        let cut = 1u16.checked_shl(u32::from(cut_version)).unwrap_or(0);
        let mut acc = self.apply_para(PCIE_GEN1_PARA_8822C, cut, true);
        let r = self.apply_para(PCIE_GEN2_PARA_8822C, cut, false);
        first(&mut acc, r);

        // The 8822CE calibrates REFCLK itself: no clock delay.
        let r = self.dbi_write8(RTK_PCIE_CLKDLY_CTRL, 0);
        first(&mut acc, r);
        let r = self.clkreq_set(false);
        first(&mut acc, r);
        let r = self.aspm_set(false);
        first(&mut acc, r);

        if rfe_option == 5 {
            self.write32_mask(REG_ANAPARSW_MAC_0, BIT_CF_L_V2, 0x1);
        }
        acc
    }
}
