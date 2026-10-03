//! Extended capabilities (xHCI 1.2 chapter 7): the BIOS-to-OS handoff
//! (USB Legacy Support, 7.1) and the Supported Protocol capabilities (7.2)
//! that say which root ports are USB 2 and which USB 3.
//!
//! The list lives in BAR 0 and is written by firmware and silicon, so it is
//! walked with a bound on both the number of entries and the offsets: a
//! cycle or a pointer past the BAR ends the walk, it never loops or reads
//! outside the mapping.

use crate::regs::{Mmio, Speed};

/// Capability IDs (Table 7-1).
pub mod id {
    pub const LEGACY: u8 = 1;
    pub const PROTOCOL: u8 = 2;
}

/// Most capabilities walked before giving up on a malformed list.
pub const MAX_CAPS: usize = 64;

/// Iterator over `(byte offset, first dword)` of each extended capability.
pub struct Caps<'a, M: Mmio> {
    mmio: &'a M,
    next: Option<usize>,
    bar_len: usize,
    left: usize,
}

/// Walk the list that starts at `first` (from [`crate::regs::extended_caps`])
/// in a BAR of `bar_len` bytes.
pub fn caps<M: Mmio>(mmio: &M, first: Option<usize>, bar_len: usize) -> Caps<'_, M> {
    Caps {
        mmio,
        next: first,
        bar_len,
        left: MAX_CAPS,
    }
}

impl<M: Mmio> Iterator for Caps<'_, M> {
    type Item = (usize, u32);

    fn next(&mut self) -> Option<(usize, u32)> {
        let at = self.next.take()?;
        if self.left == 0 || at % 4 != 0 || at.checked_add(16)? > self.bar_len {
            return None;
        }
        self.left -= 1;
        let dword = self.mmio.read32(at);
        // `next` is in dwords, relative to this capability; 0 ends the list.
        let next = ((dword >> 8) & 0xFF) as usize;
        if next != 0 {
            self.next = Some(at + next * 4);
        }
        Some((at, dword))
    }
}

/// The first capability with `cap_id`, as a byte offset.
pub fn find<M: Mmio>(mmio: &M, first: Option<usize>, bar_len: usize, cap_id: u8) -> Option<usize> {
    caps(mmio, first, bar_len)
        .find(|&(_, dword)| dword as u8 == cap_id)
        .map(|(at, _)| at)
}

/// USB Legacy Support (7.1.1, 7.1.2).
pub mod legacy {
    /// `USBLEGSUP` bit 16: the BIOS owns the controller.
    pub const BIOS_OWNED: u32 = 1 << 16;
    /// `USBLEGSUP` bit 24: the OS owns the controller.
    pub const OS_OWNED: u32 = 1 << 24;
    /// `USBLEGCTLSTS`, the dword after `USBLEGSUP`.
    pub const CTLSTS: usize = 4;
    /// `USBLEGCTLSTS` bits a write keeps: reserved and read-only ones. Every
    /// SMI enable (bits 0, 4, 13..=15) is cleared (as Linux does).
    pub const KEEP: u32 = (0x7 << 1) | (0xFF << 5) | (0x7 << 17);
    /// `USBLEGCTLSTS` SMI events (RW1C, bits 29..=31): written 1 to clear.
    pub const SMI_EVENTS: u32 = 0x7 << 29;
}

/// How the BIOS let go of the controller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Handoff {
    /// No USB Legacy Support capability: nothing to hand over.
    Absent,
    /// The BIOS did not own it; the OS-owned semaphore is now set.
    NotOwned,
    /// The BIOS released it after `polls` waits.
    Released { polls: u32 },
    /// The BIOS never released it in time; its ownership was cleared by
    /// force (what Linux does for firmware that ignores the semaphore).
    Forced,
}

/// Take the controller from the BIOS (xHCI 4.22.1): set the OS-owned
/// semaphore, wait for BIOS-owned to clear (calling `wait` between reads
/// until it returns `false`, the caller's time budget), then turn every
/// SMI off and clear the pending SMI events. Run before any other register
/// write: firmware with legacy USB emulation drives the controller from SMM
/// until then.
pub fn legacy_handoff<M: Mmio>(
    mmio: &mut M,
    first: Option<usize>,
    bar_len: usize,
    mut wait: impl FnMut() -> bool,
) -> Handoff {
    let Some(at) = find(mmio, first, bar_len, id::LEGACY) else {
        return Handoff::Absent;
    };
    let start = mmio.read32(at);
    mmio.write32(at, start | legacy::OS_OWNED);
    let mut polls = 0u32;
    let result = loop {
        let now = mmio.read32(at);
        if now & legacy::BIOS_OWNED == 0 {
            break if start & legacy::BIOS_OWNED == 0 {
                Handoff::NotOwned
            } else {
                Handoff::Released { polls }
            };
        }
        if !wait() {
            mmio.write32(at, (now & !legacy::BIOS_OWNED) | legacy::OS_OWNED);
            break Handoff::Forced;
        }
        polls = polls.saturating_add(1);
    };
    let control = mmio.read32(at + legacy::CTLSTS);
    mmio.write32(
        at + legacy::CTLSTS,
        (control & legacy::KEEP) | legacy::SMI_EVENTS,
    );
    result
}

/// Protocol Speed ID entries kept per protocol (PSIC is four bits).
const MAX_PSI: usize = 15;
/// Supported Protocol capabilities kept; more are ignored.
pub const MAX_PROTOCOLS: usize = 8;
/// No protocol covers this port.
const NONE: u8 = 0xFF;

/// One Supported Protocol capability (7.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Protocol {
    /// Major revision: 2 (USB 2) or 3 (USB 3.x).
    pub major: u8,
    pub minor: u8,
    /// First compatible port (1-based) and how many.
    pub first_port: u8,
    pub count: u8,
    /// Protocol-defined bits (for USB 2: HSO, IHI, HLC, BLC).
    pub defined: u16,
    psic: u8,
    psi: [u32; MAX_PSI],
}

/// Which protocol every root port speaks, from the capability list.
#[derive(Clone, Debug)]
pub struct Ports {
    protocols: [Option<Protocol>; MAX_PROTOCOLS],
    /// Per port (index `port - 1`): its protocol's index, or [`NONE`].
    of_port: [u8; 255],
}

impl Default for Ports {
    fn default() -> Ports {
        Ports {
            protocols: [None; MAX_PROTOCOLS],
            of_port: [NONE; 255],
        }
    }
}

impl Ports {
    /// Read every Supported Protocol capability. Ports a capability claims
    /// twice keep the first claim; a malformed one is skipped.
    pub fn read<M: Mmio>(mmio: &M, first: Option<usize>, bar_len: usize) -> Ports {
        let mut ports = Ports::default();
        let mut kept = 0;
        for (at, dword0) in caps(mmio, first, bar_len) {
            if dword0 as u8 != id::PROTOCOL || kept == MAX_PROTOCOLS {
                continue;
            }
            let name = mmio.read32(at + 4);
            let dword2 = mmio.read32(at + 8);
            let psic = (dword2 >> 28) as u8;
            // "USB " in little-endian ASCII.
            if name != 0x2042_5355 || at + 16 + usize::from(psic) * 4 > bar_len {
                continue;
            }
            let mut psi = [0u32; MAX_PSI];
            for (index, entry) in psi.iter_mut().enumerate().take(usize::from(psic)) {
                *entry = mmio.read32(at + 16 + index * 4);
            }
            let protocol = Protocol {
                major: (dword0 >> 24) as u8,
                minor: (dword0 >> 16) as u8,
                first_port: dword2 as u8,
                count: (dword2 >> 8) as u8,
                defined: ((dword2 >> 16) & 0xFFF) as u16,
                psic,
                psi,
            };
            if !matches!(protocol.major, 2 | 3) || protocol.first_port == 0 {
                continue;
            }
            ports.add(kept as u8, protocol);
            kept += 1;
        }
        ports
    }

    fn add(&mut self, index: u8, protocol: Protocol) {
        let first = usize::from(protocol.first_port);
        let last = (first + usize::from(protocol.count)).min(256);
        for port in first..last {
            if self.of_port[port - 1] == NONE {
                self.of_port[port - 1] = index;
            }
        }
        self.protocols[usize::from(index)] = Some(protocol);
    }

    /// The protocol `port` (1-based) speaks, if a capability names it.
    pub fn protocol(&self, port: u8) -> Option<&Protocol> {
        let index = *self.of_port.get(usize::from(port).checked_sub(1)?)?;
        self.protocols.get(usize::from(index))?.as_ref()
    }

    /// The USB major revision of `port`: 2 or 3, `None` when unknown.
    pub fn major(&self, port: u8) -> Option<u8> {
        self.protocol(port).map(|p| p.major)
    }

    /// Whether any port is described (QEMU and real controllers all list
    /// their protocols; an empty table means a controller that does not).
    pub fn known(&self) -> bool {
        self.protocols.iter().any(Option::is_some)
    }

    /// The speed a `PORTSC` Port Speed value (`psiv`) means on `port`: its
    /// protocol's PSI table when it has one, the default mapping (7.2.2.1.1)
    /// otherwise. A value that contradicts the port's protocol (a
    /// SuperSpeed ID on a USB 2 port) is `None`.
    pub fn speed(&self, port: u8, psiv: u8) -> Option<Speed> {
        let Some(protocol) = self.protocol(port) else {
            return Speed::from_id(u32::from(psiv));
        };
        let from_table = protocol.psi[..usize::from(protocol.psic)]
            .iter()
            .find(|&&psi| psi as u8 & 0xF == psiv)
            .map(|&psi| psi_speed(protocol.major, psi));
        let speed = match from_table {
            Some(speed) => speed,
            None => Speed::from_id(u32::from(psiv))?,
        };
        let usb3 = matches!(speed, Speed::Super | Speed::SuperPlus);
        (usb3 == (protocol.major == 3)).then_some(speed)
    }
}

/// What a Protocol Speed ID dword describes: the bit rate decides USB 2's
/// three speeds, the Link Protocol field USB 3's two.
fn psi_speed(major: u8, psi: u32) -> Speed {
    let exponent = (psi >> 4) & 0x3;
    let mantissa = u64::from(psi >> 16);
    let bits_per_second = mantissa * 1000u64.pow(exponent);
    let link_protocol = (psi >> 14) & 0x3;
    match major {
        3 if link_protocol == 0 => Speed::Super,
        3 => Speed::SuperPlus,
        _ if bits_per_second <= 1_500_000 => Speed::Low,
        _ if bits_per_second <= 12_000_000 => Speed::Full,
        _ => Speed::High,
    }
}
