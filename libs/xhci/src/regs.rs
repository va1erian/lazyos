//! xHCI registers (xHCI 1.2 chapter 5): offsets, bits and field decoders.
//!
//! The capability registers sit at the start of BAR 0; the operational ones
//! follow at `CAPLENGTH`, the runtime ones at `RTSOFF`, and the doorbells at
//! `DBOFF`. [`Mmio`] is the only way this crate touches a register, so tests
//! substitute a model controller.

/// 32- and 64-bit register access at byte offsets into one mapped region.
pub trait Mmio {
    fn read32(&self, offset: usize) -> u32;
    fn write32(&mut self, offset: usize, value: u32);

    /// 64-bit registers are written low dword first, as the spec allows for
    /// 32-bit-only hosts.
    fn write64(&mut self, offset: usize, value: u64) {
        self.write32(offset, value as u32);
        self.write32(offset + 4, (value >> 32) as u32);
    }

    fn read64(&self, offset: usize) -> u64 {
        u64::from(self.read32(offset)) | u64::from(self.read32(offset + 4)) << 32
    }
}

/// Capability registers (5.3), offsets from the BAR base.
pub mod cap {
    pub const CAPLENGTH: usize = 0x00;
    pub const HCIVERSION: usize = 0x02;
    pub const HCSPARAMS1: usize = 0x04;
    pub const HCSPARAMS2: usize = 0x08;
    pub const HCCPARAMS1: usize = 0x10;
    pub const DBOFF: usize = 0x14;
    pub const RTSOFF: usize = 0x18;
}

/// Operational registers (5.4), offsets from `CAPLENGTH`.
pub mod op {
    pub const USBCMD: usize = 0x00;
    pub const USBSTS: usize = 0x04;
    pub const PAGESIZE: usize = 0x08;
    pub const CRCR: usize = 0x18;
    pub const DCBAAP: usize = 0x30;
    pub const CONFIG: usize = 0x38;
    /// Port register sets start here, 0x10 bytes each, port 1 first.
    pub const PORTS: usize = 0x400;
    pub const PORT_STRIDE: usize = 0x10;

    /// `USBCMD` bits.
    pub const CMD_RUN: u32 = 1 << 0;
    pub const CMD_RESET: u32 = 1 << 1;
    pub const CMD_INTE: u32 = 1 << 2;

    /// `USBSTS` bits.
    pub const STS_HALTED: u32 = 1 << 0;
    pub const STS_HSE: u32 = 1 << 2;
    pub const STS_EINT: u32 = 1 << 3;
    pub const STS_PCD: u32 = 1 << 4;
    pub const STS_CNR: u32 = 1 << 11;
    pub const STS_HCE: u32 = 1 << 12;

    /// `CRCR` bit 0: the Ring Cycle State the controller starts with.
    pub const CRCR_RCS: u64 = 1;
}

/// Runtime registers (5.5), offsets from `RTSOFF`; interrupter `n` is at
/// `INTERRUPTERS + n * INTERRUPTER_STRIDE`.
pub mod rt {
    pub const INTERRUPTERS: usize = 0x20;
    pub const INTERRUPTER_STRIDE: usize = 0x20;
    pub const IMAN: usize = 0x00;
    pub const IMOD: usize = 0x04;
    pub const ERSTSZ: usize = 0x08;
    pub const ERSTBA: usize = 0x10;
    pub const ERDP: usize = 0x18;

    /// `IMAN` bits.
    pub const IMAN_IP: u32 = 1 << 0;
    pub const IMAN_IE: u32 = 1 << 1;
    /// `ERDP` bit 3: Event Handler Busy, write 1 to clear.
    pub const ERDP_EHB: u64 = 1 << 3;
}

/// `PORTSC` (5.4.8).
pub mod portsc {
    /// Current Connect Status.
    pub const CCS: u32 = 1 << 0;
    /// Port Enabled/Disabled: RW1CS, writing 1 *disables* the port.
    pub const PED: u32 = 1 << 1;
    /// Port Reset.
    pub const PR: u32 = 1 << 4;
    /// Port Link State, bits 5..=8 (USB 3 link states, USB 2 L0/L1/L2/L3).
    pub const PLS_SHIFT: u32 = 5;
    pub const PLS_MASK: u32 = 0xF << PLS_SHIFT;
    /// Port Power.
    pub const PP: u32 = 1 << 9;
    /// Port Speed, bits 10..=13.
    pub const SPEED_SHIFT: u32 = 10;
    pub const SPEED_MASK: u32 = 0xF << SPEED_SHIFT;
    /// Change bits (RW1C): connect, enable, warm reset, over-current, reset,
    /// link state, config error.
    pub const CSC: u32 = 1 << 17;
    pub const PEC: u32 = 1 << 18;
    pub const WRC: u32 = 1 << 19;
    pub const OCC: u32 = 1 << 20;
    pub const PRC: u32 = 1 << 21;
    pub const PLC: u32 = 1 << 22;
    pub const CEC: u32 = 1 << 23;
    pub const CHANGES: u32 = CSC | PEC | WRC | OCC | PRC | PLC | CEC;
    /// Warm Port Reset (USB 3 ports only; reads 0).
    pub const WPR: u32 = 1 << 31;

    /// Link states (Table 5-27) the driver acts on.
    pub mod link {
        pub const U0: u32 = 0;
        pub const DISABLED: u32 = 4;
        pub const RX_DETECT: u32 = 5;
        /// SS.Inactive: the link failed; only a warm reset recovers it.
        pub const INACTIVE: u32 = 6;
        pub const POLLING: u32 = 7;
        /// Compliance Mode: entered by a bad link-training; warm reset.
        pub const COMPLIANCE: u32 = 10;
    }

    /// The link state in `portsc`.
    pub fn link_state(portsc: u32) -> u32 {
        (portsc & PLS_MASK) >> PLS_SHIFT
    }
    /// Bits a read-modify-write must not echo back: the change bits (writing
    /// 1 clears them), `PED` (writing 1 disables) and `PR` (writing 1 resets).
    pub const NO_ECHO: u32 = CHANGES | PED | PR | (0xF << 5);

    /// The value to write to set `bits` without disturbing anything else:
    /// the current value with the dangerous bits masked off.
    pub fn set(current: u32, bits: u32) -> u32 {
        (current & !NO_ECHO) | bits
    }

    /// The value to write to acknowledge the change bits in `current`.
    pub fn ack_changes(current: u32) -> u32 {
        (current & !NO_ECHO) | (current & CHANGES)
    }
}

/// Protocol speed IDs in `PORTSC` and the slot context (default mapping, 7.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Speed {
    Full,
    Low,
    High,
    Super,
    SuperPlus,
}

impl Speed {
    /// Decode a speed ID (1..=5).
    pub fn from_id(id: u32) -> Option<Speed> {
        Some(match id {
            1 => Speed::Full,
            2 => Speed::Low,
            3 => Speed::High,
            4 => Speed::Super,
            5 => Speed::SuperPlus,
            _ => return None,
        })
    }

    /// The speed ID the slot context takes.
    pub fn id(self) -> u32 {
        match self {
            Speed::Full => 1,
            Speed::Low => 2,
            Speed::High => 3,
            Speed::Super => 4,
            Speed::SuperPlus => 5,
        }
    }

    /// The speed a `PORTSC` value reports.
    pub fn of_port(portsc: u32) -> Option<Speed> {
        Speed::from_id((portsc & portsc::SPEED_MASK) >> portsc::SPEED_SHIFT)
    }

    /// Endpoint 0's max packet size before the device descriptor is read
    /// (USB 2.0 5.5.3; 512 for SuperSpeed).
    pub fn default_max_packet0(self) -> u16 {
        match self {
            // Full speed may be 8, 16, 32 or 64: start at 64 and read only
            // the first 8 bytes, which fit in one packet whatever the real
            // size, then fix it with Evaluate Context (as Linux does).
            Speed::Low => 8,
            Speed::Full | Speed::High => 64,
            Speed::Super | Speed::SuperPlus => 512,
        }
    }
}

/// Decoded `HCSPARAMS1`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Structural {
    pub max_slots: u8,
    pub max_interrupters: u16,
    pub max_ports: u8,
}

impl Structural {
    pub fn decode(hcsparams1: u32) -> Structural {
        Structural {
            max_slots: hcsparams1 as u8,
            max_interrupters: ((hcsparams1 >> 8) & 0x7FF) as u16,
            max_ports: (hcsparams1 >> 24) as u8,
        }
    }
}

/// The number of scratchpad buffers `HCSPARAMS2` asks for (hi and lo fields).
pub fn scratchpad_count(hcsparams2: u32) -> u16 {
    let hi = (hcsparams2 >> 21) & 0x1F;
    let lo = (hcsparams2 >> 27) & 0x1F;
    ((hi << 5) | lo) as u16
}

/// Whether `HCCPARAMS1` selects 64-byte contexts (CSZ).
pub fn context_64(hccparams1: u32) -> bool {
    hccparams1 & (1 << 2) != 0
}

/// Whether the controller can address 64-bit memory (`HCCPARAMS1.AC64`).
pub fn addressing_64(hccparams1: u32) -> bool {
    hccparams1 & 1 != 0
}

/// Whether ports have power switches (`HCCPARAMS1.PPC`): their `PP` bit
/// must be set before a device can show up.
pub fn port_power_control(hccparams1: u32) -> bool {
    hccparams1 & (1 << 3) != 0
}

/// The first extended capability, as a byte offset from the BAR base
/// (`HCCPARAMS1.xECP` is in dwords; 0 means none).
pub fn extended_caps(hccparams1: u32) -> Option<usize> {
    let dwords = (hccparams1 >> 16) as usize;
    (dwords != 0).then_some(dwords * 4)
}

/// The doorbell register of `slot` (0 is the command doorbell).
pub fn doorbell(dboff: u32, slot: u8) -> usize {
    (dboff & !0x3) as usize + usize::from(slot) * 4
}

/// Endpoint `number`'s Device Context Index: `2 * number + direction`
/// (IN is 1), endpoint 0 is DCI 1.
pub fn dci(address: u8) -> u8 {
    let number = address & 0x0F;
    if number == 0 {
        1
    } else {
        number * 2 + (address >> 7)
    }
}

/// The shortest interrupt `Interval` a device gets: 2^3 microframes (1 ms),
/// however fast its descriptor asks to be polled. Input drivers run in the
/// Interactive class, so this bounds how often a hostile device can wake
/// one (1000 reports a second, each also rate-limited by its input source).
pub const MIN_INTERRUPT_INTERVAL: u8 = 3;

/// The endpoint context `Interval` for an interrupt endpoint (6.2.3.6):
/// a power-of-two exponent of 125 us microframes. Full/low speed `bInterval`
/// counts 1 ms frames; high speed and above encode `2^(bInterval-1)`
/// microframes. Never below [`MIN_INTERRUPT_INTERVAL`].
pub fn interrupt_interval(speed: Speed, b_interval: u8) -> u8 {
    match speed {
        Speed::Low | Speed::Full => {
            let microframes = u32::from(b_interval.max(1)) * 8;
            (31 - microframes.leading_zeros()).clamp(u32::from(MIN_INTERRUPT_INTERVAL), 10) as u8
        }
        _ => (b_interval.clamp(1, 16) - 1).max(MIN_INTERRUPT_INTERVAL),
    }
}
