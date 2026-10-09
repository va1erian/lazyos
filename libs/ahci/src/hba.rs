//! HBA bring-up and port discovery (AHCI 1.3.1 sections 10.1 and 10.3).

use crate::identify::Refusal;
use crate::port::{Port, PortPages};
use crate::regs::{self, bohc, cap2, ghc, Cap};
use crate::{poll, Error, Platform, MS};

/// `VS` of AHCI 1.2, the first with `CAP2` and `BOHC`.
const VS_1_2: u32 = 0x0001_0200;

/// An HBA after handoff, with `GHC.AE` set and interrupts off.
#[derive(Clone, Copy, Debug)]
pub struct Hba {
    pub cap: Cap,
    pub version: u32,
    /// Implemented ports, trusted only as far as `CAP.NP` allows.
    pub implemented: u32,
}

/// Why a port did not become a disk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Skip {
    /// Nothing attached (not an error).
    Empty,
    /// An ATAPI device (optical drive).
    Atapi,
    /// A signature this driver does not serve.
    Unknown(u32),
    /// A disk the driver refuses.
    Refused(Refusal),
    /// The port or disk did not come up.
    Failed(Error),
}

impl core::fmt::Display for Skip {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Skip::Empty => write!(f, "empty"),
            Skip::Atapi => write!(f, "ATAPI device skipped"),
            Skip::Unknown(sig) => write!(f, "unknown signature {sig:#010x}"),
            Skip::Refused(why) => write!(f, "refused: {why}"),
            Skip::Failed(error) => write!(f, "failed: {error}"),
        }
    }
}

impl Hba {
    /// Take the HBA from the firmware: BIOS/OS handoff when it exists,
    /// `GHC.AE` on, interrupts off. No HBA reset: firmware initialised the
    /// links and a reset restarts every one.
    pub fn init(platform: &dyn Platform) -> Result<Hba, Error> {
        let raw_cap = platform.read32(regs::CAP);
        let version = platform.read32(regs::VS);
        // All ones: nothing answers at this address.
        if raw_cap == u32::MAX && version == u32::MAX {
            return Err(Error::Fatal);
        }
        if version >= VS_1_2 && platform.read32(regs::CAP2) & cap2::BOH != 0 {
            Self::handoff(platform);
        }
        let control = platform.read32(regs::GHC);
        platform.write32(regs::GHC, (control | ghc::AE) & !ghc::IE);
        let cap = Cap::decode(raw_cap);
        let mut implemented = platform.read32(regs::PI);
        // Keep at most `CAP.NP + 1` of the set bits, lowest first.
        let mut kept = 0u32;
        for _ in 0..cap.ports {
            let low = implemented.isolate_lowest_one();
            kept |= low;
            implemented &= !low;
        }
        Ok(Hba {
            cap,
            version,
            implemented: kept,
        })
    }

    /// 10.6.3: claim ownership and wait for the BIOS to let go. A BIOS that
    /// never does is overridden after the bound; the ports are then taken
    /// the same way.
    fn handoff(platform: &dyn Platform) {
        let control = platform.read32(regs::BOHC);
        platform.write32(regs::BOHC, control | bohc::OOS);
        let released = poll(platform, 25 * MS, || {
            platform.read32(regs::BOHC) & bohc::BOS == 0
        });
        if !released {
            return;
        }
        // The BIOS may need up to two seconds to finish its commands.
        if platform.read32(regs::BOHC) & bohc::BB != 0 {
            poll(platform, 2000 * MS, || {
                platform.read32(regs::BOHC) & bohc::BB == 0
            });
        }
    }

    /// The highest implemented port, if any.
    pub fn highest_port(&self) -> Option<usize> {
        (self.implemented != 0).then(|| 31 - self.implemented.leading_zeros() as usize)
    }

    /// Whether port `index` is implemented.
    pub fn has_port(&self, index: usize) -> bool {
        index < regs::MAX_PORTS && self.implemented & (1 << index) != 0
    }

    /// Bring port `index` up and identify its disk.
    pub fn open_port(
        &self,
        platform: &dyn Platform,
        index: usize,
        pages: PortPages,
    ) -> Result<Port, Skip> {
        if !self.has_port(index) {
            return Err(Skip::Empty);
        }
        Port::open(platform, self, index, pages)
    }
}
