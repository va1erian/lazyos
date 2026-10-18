//! What the chip says about itself before anything is powered: the system
//! configuration register and a fixed register dump, so a failed bring-up on
//! the box (no serial port) is diagnosable from `dbgd`'s log alone.
//!
//! Register offsets and bits are upstream's (`reg.h`; the decoding is
//! `rtw_chip_parameter_setup` in `main.c`, Linux d6d3455d8751, taken under
//! BSD-3-Clause like `libs/rtw88`).

use alloc::format;
use alloc::string::String;
use core::fmt::Write;

use rtw88::pci::Mmio;

use super::trace;

/// `REG_SYS_CFG1`: chip version, cut, RF type and the test-chip bit.
pub(super) const REG_SYS_CFG1: u32 = 0x00F0;
/// `BIT_RTL_ID`: set on a test (non-production) chip.
const BIT_RTL_ID: u32 = 1 << 23;
/// `BIT_RF_TYPE_ID`: set on a 2T2R chip.
const BIT_RF_TYPE_ID: u32 = 1 << 27;
/// `BIT_SHIFT_CHIP_VER` / `BIT_MASK_CHIP_VER`: the cut (0 is A).
const CHIP_VER_SHIFT: u32 = 12;
const CHIP_VER_MASK: u32 = 0xF;

/// The always-on system registers (`0x00..0xFF`, readable before power-on,
/// as upstream reads `REG_SYS_CFG1` at probe): `SYS_ISO_CTRL`/`FUNC_EN`,
/// `SYS_PW_CTRL`, `SYS_CLK_CTRL`, `RSV_CTRL`/`RF_CTRL`, `AFE_CTRL1`,
/// `EFUSE_CTRL`, `GPIO_MUXCFG`, `LED_CFG`, `PAD_CTRL1`, `WL_BT_PWR_CTRL`,
/// `HCI_OPT_CTRL`, `MCUFW_CTRL`, `HIMR0`, `HISR0`, `SYS_CFG1`,
/// `SYS_STATUS1`, `SYS_CFG2`.
pub(super) const SYSTEM_REGS: &[u32] = &[
    0x0000, 0x0004, 0x0008, 0x001C, 0x0024, 0x0030, 0x0040, 0x004C, 0x0064, 0x0068, 0x0074, 0x0080,
    0x00B0, 0x00B4, 0x00F0, 0x00F4, 0x00FC,
];

/// The MAC registers worth reading once the MAC is powered (the bring-up's
/// failure dump adds them): `CR`, `HMETFR`, `TXDMA_STATUS`, `RXDMA_STATUS`,
/// the PCIe DBI/MDIO block, `FWHW_TXQ_CTRL`, `CPU_DMEM_CON`.
pub(super) const MAC_REGS: &[u32] = &[
    0x0100, 0x01CC, 0x0210, 0x0288, 0x03E8, 0x03F0, 0x03F4, 0x03F8, 0x0420, 0x1080,
];

/// The chip identity `REG_SYS_CFG1` holds.
#[derive(Clone, Copy, Debug)]
pub(super) struct Identity {
    pub(super) sys_cfg1: u32,
    /// The cut: 0 is A, 1 is B, ...
    pub(super) cut: u8,
    /// 2T2R (true) or 1T1R.
    pub(super) two_by_two: bool,
    /// A production chip (upstream `mp_chip`), not a test sample.
    pub(super) production: bool,
}

/// Read and decode `REG_SYS_CFG1`.
pub(super) fn identify(regs: &impl Mmio) -> Identity {
    let sys_cfg1 = regs.read32(REG_SYS_CFG1);
    Identity {
        sys_cfg1,
        cut: (sys_cfg1 >> CHIP_VER_SHIFT & CHIP_VER_MASK) as u8,
        two_by_two: sys_cfg1 & BIT_RF_TYPE_ID != 0,
        production: sys_cfg1 & BIT_RTL_ID == 0,
    }
}

impl Identity {
    /// `A`, `B`, ... for the cut.
    pub(super) fn cut_letter(&self) -> char {
        char::from(b'A' + self.cut)
    }

    /// Whether the read looks like a dead bus (all ones: the card is gone
    /// or not decoding; all zeros: the BAR maps nothing).
    pub(super) fn bus_dead(&self) -> bool {
        self.sys_cfg1 == u32::MAX || self.sys_cfg1 == 0
    }

    /// `WIFID:CHIP sys_cfg1=0x... cut=B rf=2T2R production=1`.
    pub(super) fn report(&self) {
        let cut = format!("{}", self.cut_letter());
        trace::event(
            "CHIP",
            &[
                ("sys_cfg1", &trace::Hex32(self.sys_cfg1)),
                ("cut", &cut),
                ("rf", &if self.two_by_two { "2T2R" } else { "1T1R" }),
                ("production", &u8::from(self.production)),
            ],
        );
    }
}

/// Print `regs` as `WIFID:REGS <label> 00f0=0x... ...` lines, six to a line.
pub(super) fn dump(label: &str, regs: &impl Mmio, offsets: &[u32]) {
    for chunk in offsets.chunks(6) {
        let mut text = String::new();
        for &offset in chunk {
            let _ = write!(text, " {offset:04x}={:#010x}", regs.read32(offset));
        }
        trace::text(format_args!("REGS {label}{text}"));
    }
}
