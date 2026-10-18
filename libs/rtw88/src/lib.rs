//! The logic of the Realtek RTL8822CE Wi-Fi driver, as host-testable
//! `no_std` code (docs/wifi-rtw88-plan.md).
//!
//! The Kaby Lake box's Wi-Fi card (`10ec:c822`, docs/compat/kabylake) is driven by
//! Linux's `rtw88`, which is dual-licensed `GPL-2.0 OR BSD-3-Clause`; this
//! crate is a Rust translation of the parts a station needs, taken under
//! BSD-3-Clause, with every module naming the upstream file and functions it
//! follows (pinned commit in [`UPSTREAM`]). Upstream: Copyright(c)
//! 2018-2019 Realtek Corporation. `THIRD_PARTY.md` carries the full
//! BSD-3-Clause notice and every image ships it at `/docs/os/THIRD_PARTY.md`. Never read `mac80211`/`cfg80211` (GPL-2.0-only) while working here.
//!
//! The crate mirrors upstream's split so a reader can hold the two side by
//! side:
//!
//! | Here | Upstream | What |
//! |---|---|---|
//! | [`hci`] | `hci.h` | the bus seam: register I/O, TX queues, reserved page, H2C |
//! | [`dev`] | `main.h` (`struct rtw_dev`) | the device state every module reads |
//! | [`bringup`] | `main.c` | `power_on`, `core_start`, the bring-up order |
//! | [`pwrseq`], [`mac`] | `mac.c` | power sequences, MAC init, TRX/queue config |
//! | [`fw`] | `fw.c` | firmware header, download, H2C commands, C2H events |
//! | [`efuse`] | `efuse.c` | physical to logical efuse map |
//! | [`sec`] | `sec.c` | the security CAM |
//! | [`tx`], [`rx`] | `tx.c`, `rx.c` | TX packet descriptors, RX descriptors |
//! | [`pci`] | `pci.c`, `pci.h` | PCIe buffer-descriptor rings, interrupts, DBI/MDIO |
//! | [`phy`] | `phy.c` | table loading, RF access, TX power |
//! | [`rtw8822c`] | `rtw8822c.c`, `rtw8822c_table.c` | the chip: efuse layout, BB/RF init, channel, calibration |
//! | [`coex`] | `coex.c` | the minimum to give the shared antenna to Wi-Fi |
//! | [`station`] | `mac80211.c`, `main.c` | the station facade a soft-MAC drives: port, BSSID, filters, association, keys, TX packets |
//!
//! Nothing here takes a syscall: the binary (`wifid`) claims the function,
//! maps BAR 2, allocates DMA and hands this crate an [`hci::Hci`]. The host
//! tests drive it against [`fake`] models.
//!
//! The station logic (scan parsing, authentication, the 4-way handshake) is
//! not here: it lives in `wlanmd` over `os.lazy.net.wifi.hw.v1`. This crate
//! only moves frames and programs the radio.

#![no_std]

extern crate alloc;

#[cfg(any(test, feature = "fuzz"))]
extern crate std;

pub mod bringup;
pub mod coex;
pub mod dev;
pub mod efuse;
pub mod error;
pub mod fw;
pub mod hci;
pub mod mac;
pub mod pci;
pub mod phy;
pub mod pwrseq;
pub mod rtw8822c;
pub mod rx;
pub mod sec;
pub mod station;
pub mod tx;

#[cfg(test)]
pub mod fake;
#[cfg(test)]
mod testmodel;

pub use dev::Dev;
pub use error::Error;
pub use hci::Hci;

/// The Linux tree this crate was translated from (torvalds/linux master on
/// 2026-10-11). Keep it in step with any re-read of upstream.
pub const UPSTREAM: &str = "d6d3455d8751854e0d1820f14184682e23ac4475";

/// Realtek's PCI vendor id.
pub const VENDOR: u16 = 0x10EC;

/// The PCI device id of the RTL8822CE.
pub const DEVICE_8822CE: u16 = 0xC822;

/// The functions this driver matches by id (`devmatch` keeps its manifest
/// equal to this by test).
pub const DEVICES: &[(u16, &str)] = &[(DEVICE_8822CE, "RTL8822CE")];

/// The model name of a function this driver may drive, `None` otherwise.
pub fn model(vendor: u16, device: u16) -> Option<&'static str> {
    if vendor != VENDOR {
        return None;
    }
    DEVICES
        .iter()
        .find(|(id, _)| *id == device)
        .map(|(_, name)| *name)
}

/// The firmware file, relative to the firmware tree (`fhs::FIRMWARE`).
pub const FIRMWARE: &str = "rtw88/rtw8822c_fw.bin";
