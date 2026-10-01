//! Transfer Request Blocks (xHCI 6.4): the 16-byte unit of every ring.
//!
//! A TRB is a 64-bit parameter, a 32-bit status and a 32-bit control word
//! whose bit 0 is the cycle bit and bits 10..=15 the type. Builders here
//! leave the cycle bit clear; the producer ring stamps it.

/// One TRB, in the order it sits in memory.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Trb {
    pub parameter: u64,
    pub status: u32,
    pub control: u32,
}

/// Bytes per TRB; rings and pointers are aligned to it.
pub const TRB_BYTES: u64 = 16;

/// Control word bits.
pub const CYCLE: u32 = 1 << 0;
/// Link TRB: toggle the consumer's cycle state when following it.
pub const TOGGLE_CYCLE: u32 = 1 << 1;
/// Interrupt on Short Packet.
pub const ISP: u32 = 1 << 2;
/// Chain bit: the next TRB is part of the same transfer descriptor.
pub const CHAIN: u32 = 1 << 4;
/// Interrupt On Completion.
pub const IOC: u32 = 1 << 5;
/// Immediate Data: the parameter field holds the data (setup stage).
pub const IDT: u32 = 1 << 6;
/// Block Set Address Request (Address Device command).
pub const BSR: u32 = 1 << 9;
const TYPE_SHIFT: u32 = 10;

/// TRB types (6.4.6).
pub mod kind {
    pub const NORMAL: u8 = 1;
    pub const SETUP_STAGE: u8 = 2;
    pub const DATA_STAGE: u8 = 3;
    pub const STATUS_STAGE: u8 = 4;
    pub const LINK: u8 = 6;
    pub const ENABLE_SLOT: u8 = 9;
    pub const DISABLE_SLOT: u8 = 10;
    pub const ADDRESS_DEVICE: u8 = 11;
    pub const CONFIGURE_ENDPOINT: u8 = 12;
    pub const EVALUATE_CONTEXT: u8 = 13;
    pub const RESET_ENDPOINT: u8 = 14;
    pub const STOP_ENDPOINT: u8 = 15;
    pub const SET_TR_DEQUEUE: u8 = 16;
    pub const NO_OP_COMMAND: u8 = 23;
    pub const TRANSFER_EVENT: u8 = 32;
    pub const COMMAND_COMPLETION: u8 = 33;
    pub const PORT_STATUS_CHANGE: u8 = 34;
    pub const HOST_CONTROLLER: u8 = 37;
}

/// Completion codes (6.4.5) `usbd` acts on.
pub mod code {
    pub const SUCCESS: u8 = 1;
    pub const DATA_BUFFER: u8 = 2;
    pub const BABBLE: u8 = 3;
    pub const TRANSACTION: u8 = 4;
    pub const TRB: u8 = 5;
    pub const STALL: u8 = 6;
    pub const SHORT_PACKET: u8 = 13;
    pub const STOPPED: u8 = 26;
}

impl Trb {
    /// The TRB type.
    pub fn kind(&self) -> u8 {
        ((self.control >> TYPE_SHIFT) & 0x3F) as u8
    }

    pub fn cycle(&self) -> bool {
        self.control & CYCLE != 0
    }

    /// An event's completion code.
    pub fn completion_code(&self) -> u8 {
        (self.status >> 24) as u8
    }

    /// An event's slot ID.
    pub fn slot(&self) -> u8 {
        (self.control >> 24) as u8
    }

    /// A transfer event's endpoint ID (DCI).
    pub fn endpoint(&self) -> u8 {
        ((self.control >> 16) & 0x1F) as u8
    }

    /// A transfer event's residual: bytes *not* transferred.
    pub fn residual(&self) -> u32 {
        self.status & 0x00FF_FFFF
    }

    /// A port status change event's port number (1-based).
    pub fn port(&self) -> u8 {
        (self.parameter >> 24) as u8
    }

    fn of(kind: u8, parameter: u64, status: u32, flags: u32) -> Trb {
        Trb {
            parameter,
            status,
            control: (u32::from(kind) << TYPE_SHIFT) | flags,
        }
    }
}

/// A Link TRB pointing back to `segment` (with Toggle Cycle on the last one).
pub fn link(segment: u64, toggle: bool) -> Trb {
    Trb::of(
        kind::LINK,
        segment,
        0,
        if toggle { TOGGLE_CYCLE } else { 0 },
    )
}

/// Enable Slot (slot type 0, USB).
pub fn enable_slot() -> Trb {
    Trb::of(kind::ENABLE_SLOT, 0, 0, 0)
}

/// Disable Slot.
pub fn disable_slot(slot: u8) -> Trb {
    Trb::of(kind::DISABLE_SLOT, 0, 0, u32::from(slot) << 24)
}

/// No Op command (a ring liveness test).
pub fn no_op_command() -> Trb {
    Trb::of(kind::NO_OP_COMMAND, 0, 0, 0)
}

/// Address Device with the input context at `input`; `bsr` skips the
/// SET_ADDRESS request (the slot goes to Default instead of Addressed).
pub fn address_device(input: u64, slot: u8, bsr: bool) -> Trb {
    let flags = (u32::from(slot) << 24) | if bsr { BSR } else { 0 };
    Trb::of(kind::ADDRESS_DEVICE, input, 0, flags)
}

/// Configure Endpoint with the input context at `input`.
pub fn configure_endpoint(input: u64, slot: u8) -> Trb {
    Trb::of(kind::CONFIGURE_ENDPOINT, input, 0, u32::from(slot) << 24)
}

/// Evaluate Context (e.g. endpoint 0's max packet size after the descriptor).
pub fn evaluate_context(input: u64, slot: u8) -> Trb {
    Trb::of(kind::EVALUATE_CONTEXT, input, 0, u32::from(slot) << 24)
}

/// Stop Endpoint `dci` of `slot` (before freeing its ring).
pub fn stop_endpoint(slot: u8, dci: u8) -> Trb {
    let flags = (u32::from(slot) << 24) | (u32::from(dci) << 16);
    Trb::of(kind::STOP_ENDPOINT, 0, 0, flags)
}

/// Reset Endpoint `dci` of `slot` (after a stall).
pub fn reset_endpoint(slot: u8, dci: u8) -> Trb {
    let flags = (u32::from(slot) << 24) | (u32::from(dci) << 16);
    Trb::of(kind::RESET_ENDPOINT, 0, 0, flags)
}

/// A USB control request (USB 2.0 9.3), the 8 bytes of a setup packet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SetupPacket {
    pub request_type: u8,
    pub request: u8,
    pub value: u16,
    pub index: u16,
    pub length: u16,
}

impl SetupPacket {
    /// Whether the data stage (if any) is device-to-host.
    pub fn is_in(&self) -> bool {
        self.request_type & 0x80 != 0
    }

    /// The 8 bytes on the wire (USB 2.0 Table 9-2, little-endian fields).
    pub fn immediate_bytes(&self) -> [u8; 8] {
        let [value_lo, value_hi] = self.value.to_le_bytes();
        let [index_lo, index_hi] = self.index.to_le_bytes();
        let [length_lo, length_hi] = self.length.to_le_bytes();
        [
            self.request_type,
            self.request,
            value_lo,
            value_hi,
            index_lo,
            index_hi,
            length_lo,
            length_hi,
        ]
    }

    fn immediate(&self) -> u64 {
        u64::from_le_bytes(self.immediate_bytes())
    }
}

/// Standard and HID class requests `usbd` issues.
pub mod request {
    use super::SetupPacket;

    pub const GET_DESCRIPTOR: u8 = 6;
    pub const SET_CONFIGURATION: u8 = 9;
    pub const HID_SET_IDLE: u8 = 0x0A;
    pub const HID_SET_PROTOCOL: u8 = 0x0B;

    /// GET_DESCRIPTOR(`kind`, `index`) for `length` bytes.
    pub fn get_descriptor(kind: u8, index: u8, length: u16) -> SetupPacket {
        SetupPacket {
            request_type: 0x80,
            request: GET_DESCRIPTOR,
            value: u16::from(kind) << 8 | u16::from(index),
            index: 0,
            length,
        }
    }

    /// GET_DESCRIPTOR(REPORT) of HID `interface`, for `length` bytes: the
    /// recipient is the interface, not the device (HID 1.11 7.1.1).
    pub fn get_report_descriptor(interface: u8, length: u16) -> SetupPacket {
        SetupPacket {
            request_type: 0x81,
            request: GET_DESCRIPTOR,
            value: 0x22 << 8,
            index: u16::from(interface),
            length,
        }
    }

    pub fn set_configuration(value: u8) -> SetupPacket {
        SetupPacket {
            request_type: 0x00,
            request: SET_CONFIGURATION,
            value: u16::from(value),
            index: 0,
            length: 0,
        }
    }

    /// HID SET_PROTOCOL: 0 boot, 1 report.
    pub fn set_protocol(interface: u8, boot: bool) -> SetupPacket {
        SetupPacket {
            request_type: 0x21,
            request: HID_SET_PROTOCOL,
            value: u16::from(!boot),
            index: u16::from(interface),
            length: 0,
        }
    }

    /// HID SET_IDLE(0): report only on change.
    pub fn set_idle(interface: u8) -> SetupPacket {
        SetupPacket {
            request_type: 0x21,
            request: HID_SET_IDLE,
            value: 0,
            index: u16::from(interface),
            length: 0,
        }
    }
}

/// The TRBs of one control transfer: setup, optional data at `buffer`, and
/// status (the status stage's direction is opposite the data's, IN when
/// there is no data; it interrupts on completion).
pub fn control_transfer(setup: &SetupPacket, buffer: u64) -> ([Trb; 3], usize) {
    let has_data = setup.length != 0;
    // Transfer Type: 0 no data, 2 OUT data, 3 IN data.
    let trt = match (has_data, setup.is_in()) {
        (false, _) => 0,
        (true, false) => 2,
        (true, true) => 3,
    };
    let setup_trb = Trb::of(kind::SETUP_STAGE, setup.immediate(), 8, IDT | (trt << 16));
    let status_in = !has_data || !setup.is_in();
    let status = Trb::of(kind::STATUS_STAGE, 0, 0, IOC | u32::from(status_in) << 16);
    if has_data {
        let dir = u32::from(setup.is_in()) << 16;
        let data = Trb::of(kind::DATA_STAGE, buffer, u32::from(setup.length), dir);
        ([setup_trb, data, status], 3)
    } else {
        ([setup_trb, status, Trb::default()], 2)
    }
}

/// A Normal TRB for an interrupt-IN transfer of `length` bytes into
/// `buffer`, interrupting on completion and on a short packet (a report
/// shorter than `wMaxPacketSize` is normal).
pub fn interrupt_in(buffer: u64, length: u16) -> Trb {
    Trb::of(kind::NORMAL, buffer, u32::from(length), IOC | ISP)
}
