//! Device and input contexts (xHCI 6.2): what the driver tells the
//! controller about a slot and its endpoints, and what it reads back.
//!
//! A context is 32 or 64 bytes (`HCCPARAMS1.CSZ`); only the first eight
//! dwords carry fields either way. An input context is the input control
//! context, the slot context, then 31 endpoint contexts (endpoint DCI `n` is
//! context `n + 1`); a device context is the same without the input control
//! context.

use crate::regs::Speed;
use crate::Error;

/// Contexts in an input context (control + slot + 31 endpoints).
pub const INPUT_CONTEXTS: usize = 33;
/// Contexts in a device (output) context.
pub const DEVICE_CONTEXTS: usize = 32;

/// Endpoint types (6.2.3, Table 6-9).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EndpointType {
    Control,
    InterruptIn,
    InterruptOut,
    BulkIn,
    BulkOut,
}

impl EndpointType {
    fn value(self) -> u32 {
        match self {
            EndpointType::InterruptOut => 3,
            EndpointType::BulkOut => 2,
            EndpointType::Control => 4,
            EndpointType::BulkIn => 6,
            EndpointType::InterruptIn => 7,
        }
    }
}

/// Slot context fields the driver sets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SlotContext {
    /// Route string (0 for a device on a root port; [`crate::route`]).
    pub route: u32,
    pub speed: Speed,
    /// The highest valid endpoint DCI (1 for just endpoint 0).
    pub entries: u8,
    /// Root hub port number, 1-based.
    pub root_port: u8,
    /// The transaction translator a low- or full-speed device behind a
    /// high-speed hub is reached through (6.2.2, TT Hub Slot ID and Port).
    pub tt: Option<Tt>,
    /// Set when the device is itself a hub (Hub, Number of Ports, TTT, MTT).
    pub hub: Option<HubSlot>,
}

/// A transaction translator: the high-speed hub's slot and the port of it
/// the device hangs off, and whether that hub runs one TT per port.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tt {
    pub hub_slot: u8,
    pub port: u8,
    pub multi: bool,
}

/// What the slot context of a hub declares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HubSlot {
    pub ports: u8,
    /// A high-speed hub with its multiple-TT interface selected.
    pub multi_tt: bool,
    /// TT Think Time, `wHubCharacteristics` bits 5..=6 (high-speed hubs).
    pub think_time: u8,
}

impl SlotContext {
    /// A device on root port `root_port` with only endpoint 0.
    pub fn root(speed: Speed, root_port: u8) -> SlotContext {
        SlotContext {
            route: 0,
            speed,
            entries: 1,
            root_port,
            tt: None,
            hub: None,
        }
    }

    fn dwords(&self) -> Result<[u32; 3], Error> {
        if self.route >= 1 << 20 || !(1..=31).contains(&self.entries) || self.root_port == 0 {
            return Err(Error::BadArgument);
        }
        let mut dwords = [
            self.route | self.speed.id() << 20 | u32::from(self.entries) << 27,
            u32::from(self.root_port) << 16,
            0,
        ];
        if let Some(tt) = self.tt {
            if tt.hub_slot == 0 || tt.port == 0 {
                return Err(Error::BadArgument);
            }
            dwords[0] |= u32::from(tt.multi) << 25;
            dwords[2] |= u32::from(tt.hub_slot) | u32::from(tt.port) << 8;
        }
        if let Some(hub) = self.hub {
            if hub.ports == 0 || hub.think_time > 3 {
                return Err(Error::BadArgument);
            }
            dwords[0] |= 1 << 26 | u32::from(hub.multi_tt) << 25;
            dwords[1] |= u32::from(hub.ports) << 24;
            dwords[2] |= u32::from(hub.think_time) << 16;
        }
        Ok(dwords)
    }
}

/// Endpoint context fields the driver sets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EndpointContext {
    pub kind: EndpointType,
    pub max_packet: u16,
    /// The `Interval` exponent ([`crate::regs::interrupt_interval`]); 0 for
    /// control.
    pub interval: u8,
    /// The transfer ring's dequeue pointer with its cycle state in bit 0
    /// ([`crate::ring::ProducerRing::dequeue_pointer`]).
    pub dequeue: u64,
    /// Average TRB length: 8 for control, the report size for interrupt,
    /// 3072 for bulk (4.14.1.1).
    pub average_trb: u16,
    /// Max Burst Size: extra packets per burst (high-speed periodic: the
    /// `wMaxPacketSize` transaction bits; SuperSpeed: the companion's
    /// `bMaxBurst`), 0 otherwise.
    pub max_burst: u8,
    /// Max ESIT Payload: bytes per service interval of a periodic endpoint
    /// (`max_packet * (max_burst + 1)`, or the SuperSpeed companion's
    /// `wBytesPerInterval`); 0 for control and bulk.
    pub max_esit: u32,
}

impl EndpointContext {
    /// Endpoint 0 with `max_packet` bytes per packet.
    pub fn control(max_packet: u16, dequeue: u64) -> EndpointContext {
        EndpointContext {
            kind: EndpointType::Control,
            max_packet,
            interval: 0,
            dequeue,
            average_trb: 8,
            max_burst: 0,
            max_esit: 0,
        }
    }

    /// A bulk endpoint (`kind` is [`EndpointType::BulkIn`] or `BulkOut`).
    pub fn bulk(
        kind: EndpointType,
        max_packet: u16,
        max_burst: u8,
        dequeue: u64,
    ) -> EndpointContext {
        EndpointContext {
            kind,
            max_packet,
            interval: 0,
            dequeue,
            average_trb: 3072,
            max_burst,
            max_esit: 0,
        }
    }

    /// A periodic (interrupt) endpoint; the ESIT payload is one burst of
    /// full packets unless the caller sets a SuperSpeed `wBytesPerInterval`.
    pub fn interrupt(
        kind: EndpointType,
        max_packet: u16,
        max_burst: u8,
        interval: u8,
        dequeue: u64,
    ) -> EndpointContext {
        EndpointContext {
            kind,
            max_packet,
            interval,
            dequeue,
            average_trb: max_packet.min(1024),
            max_burst,
            max_esit: u32::from(max_packet) * (u32::from(max_burst) + 1),
        }
    }
}

/// An input context being built in a caller-provided dword buffer.
pub struct InputContext<'a> {
    dwords: &'a mut [u32],
    /// Dwords per context: 8 or 16.
    stride: usize,
}

impl<'a> InputContext<'a> {
    /// Clear `buffer` and lay an input context over it.
    pub fn new(buffer: &'a mut [u32], context_64: bool) -> Result<InputContext<'a>, Error> {
        let stride = if context_64 { 16 } else { 8 };
        if buffer.len() < INPUT_CONTEXTS * stride {
            return Err(Error::BadBuffer);
        }
        buffer.fill(0);
        Ok(InputContext {
            dwords: buffer,
            stride,
        })
    }

    /// Set the Add flag for context `dci` (0 is the slot context).
    pub fn add(&mut self, dci: u8) -> Result<(), Error> {
        if dci > 31 {
            return Err(Error::BadArgument);
        }
        self.dwords[1] |= 1 << dci;
        Ok(())
    }

    /// The add flags (input control context dword 1).
    pub fn added(&self) -> u32 {
        self.dwords[1]
    }

    /// Write the slot context and add it.
    pub fn slot(&mut self, slot: &SlotContext) -> Result<(), Error> {
        let dwords = slot.dwords()?;
        self.context(1)[..3].copy_from_slice(&dwords);
        self.add(0)
    }

    /// Write endpoint `dci`'s context and add it.
    pub fn endpoint(&mut self, dci: u8, ep: &EndpointContext) -> Result<(), Error> {
        if !(1..=31).contains(&dci)
            || ep.max_packet == 0
            || ep.max_packet > 1024
            || ep.interval > 15
            || ep.dequeue & 0xE != 0
            || ep.max_esit >= 1 << 24
        {
            return Err(Error::BadArgument);
        }
        let ctx = self.context(usize::from(dci) + 1);
        // Max ESIT Payload Hi in bits 24..=31, Interval in 16..=23.
        ctx[0] = u32::from(ep.interval) << 16 | (ep.max_esit >> 16) << 24;
        // Error Count 3: retry a failed transaction three times.
        ctx[1] = 3 << 1
            | ep.kind.value() << 3
            | u32::from(ep.max_burst) << 8
            | u32::from(ep.max_packet) << 16;
        ctx[2] = ep.dequeue as u32;
        ctx[3] = (ep.dequeue >> 32) as u32;
        ctx[4] = u32::from(ep.average_trb) | (ep.max_esit & 0xFFFF) << 16;
        self.add(dci)
    }

    /// Set only endpoint 0's max packet size (Evaluate Context after the
    /// first eight bytes of the device descriptor).
    pub fn ep0_max_packet(&mut self, max_packet: u16) -> Result<(), Error> {
        if max_packet == 0 || max_packet > 512 {
            return Err(Error::BadArgument);
        }
        let ctx = self.context(2);
        ctx[1] = (ctx[1] & 0xFFFF) | u32::from(max_packet) << 16;
        self.add(1)
    }

    fn context(&mut self, index: usize) -> &mut [u32] {
        &mut self.dwords[index * self.stride..(index + 1) * self.stride]
    }

    /// The raw dwords (tests).
    pub fn dwords(&self) -> &[u32] {
        self.dwords
    }
}

/// Slot states (6.2.2, Slot State field).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlotState {
    DisabledOrEnabled,
    Default,
    Addressed,
    Configured,
    Reserved,
}

/// The slot state and USB address the controller recorded in the output
/// slot context (`device` is the device context's first dwords).
pub fn slot_state(device: &[u32]) -> Option<(SlotState, u8)> {
    let dword3 = *device.get(3)?;
    let state = match dword3 >> 27 {
        0 => SlotState::DisabledOrEnabled,
        1 => SlotState::Default,
        2 => SlotState::Addressed,
        3 => SlotState::Configured,
        _ => SlotState::Reserved,
    };
    Some((state, dword3 as u8))
}
