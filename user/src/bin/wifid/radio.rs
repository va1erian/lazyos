//! The radio: where `libs/rtw88`'s bring-up runs on the real card
//! (docs/wifi-rtw88-plan.md R1 and R3).
//!
//! **First light.** Nothing is powered on yet: [`bring_up`] reports what the
//! full driver will be handed and returns [`State::Pending`]. The chip
//! library (power sequences, firmware download, MAC/BB/RF init, PCIe rings)
//! is being written in `libs/rtw88`; once it lands, the body of
//! [`bring_up`] becomes, in upstream's order (`rtw_power_on`,
//! `rtw_core_start`):
//!
//! 1. an `rtw88::pci::Pci` over `bus.regs` (`Mmio`), `bus.dma` (`DmaAlloc`)
//!    and `bus.delay_us`;
//! 2. `rtw88::bringup` power-on with `firmware` (the bytes `fwload` read),
//!    printing `WIFID:FW:READY` when the firmware answers and
//!    `WIFID:MAC:<addr>` from the efuse;
//! 3. on any `rtw88::Error`, `Err(Failure::chip(step, error))`: `wifid.rs`
//!    prints `WIFID:FAIL:<step>` and [`failure_dump`]'s registers, then
//!    parks with the claim held so `dbgd` can still look.
//!
//! Interrupts reach [`Radio::interrupt`] from the serve loop (MSI is armed
//! before this runs; the chip raises nothing until `HIMR` is written).

use alloc::string::String;
use alloc::vec::Vec;

use rtw88::pci::Mmio;

use super::bus::{Bar, Dma};
use super::{chip, trace};

/// What the chip library is given: the card's registers, DMA memory and a
/// delay. Built once the card is claimed and mapped.
pub(super) struct Bus {
    pub(super) regs: Bar,
    pub(super) dma: Dma,
    pub(super) delay_us: fn(u32),
}

/// A bring-up step that failed, for `WIFID:FAIL:<step>`.
pub(super) struct Failure {
    pub(super) step: &'static str,
    pub(super) detail: String,
}

/// Where the radio stands.
pub(super) enum State {
    /// First light: identified, firmware loaded, not powered on.
    Pending,
}

/// The radio for the serve loop.
pub(super) struct Radio {
    pub(super) bus: Bus,
    pub(super) state: State,
    /// Interrupt messages received.
    pub(super) interrupts: u64,
}

/// Bring the radio up as far as this build of `wifid` knows how.
pub(super) fn bring_up(bus: Bus, firmware: Vec<u8>) -> Result<Radio, (Bus, Failure)> {
    // THE BRING-UP SLOTS IN HERE (see the module docs): power on, download
    // `firmware`, read the efuse, report `WIFID:FW:READY` and `WIFID:MAC:`.
    trace::event(
        "RADIO:PENDING",
        &[
            ("firmware_bytes", &firmware.len()),
            ("dma_blocks", &bus.dma.blocks()),
            ("reason", &"chip bring-up not in this build (libs/rtw88 R1)"),
        ],
    );
    // A tick of the bus delay, so the seam the library will use is proven.
    (bus.delay_us)(10);
    Ok(Radio {
        bus,
        state: State::Pending,
        interrupts: 0,
    })
}

impl Radio {
    /// One interrupt message arrived: count it. (With the chip unpowered
    /// none should; the bring-up will read and clear `HISR` here.)
    pub(super) fn interrupt(&mut self) {
        self.interrupts += 1;
        if self.interrupts == 1 {
            let state = match self.state {
                State::Pending => "pending",
            };
            trace::event(
                "IRQ:FIRST",
                &[
                    ("state", &state),
                    ("sys_cfg1", &trace::Hex32(self.bus.regs.read32(chip::REG_SYS_CFG1))),
                ],
            );
        }
    }
}

/// The register dump printed after a failed bring-up step: the always-on
/// system registers and the MAC's.
pub(super) fn failure_dump(regs: &Bar) {
    chip::dump("fail", regs, chip::SYSTEM_REGS);
    chip::dump("fail", regs, chip::MAC_REGS);
}
