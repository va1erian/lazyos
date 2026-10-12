//! What a received frame is, read from its first octets only: the first
//! decision a soft-MAC makes for every frame the radio hands it, before it
//! parses anything further.

use crate::data::{fc, TYPE_CONTROL, TYPE_DATA, TYPE_MGMT};

/// A received frame's class.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameClass {
    /// A management frame of this subtype (`frame::SUB_*`); `protected` when
    /// its Protected bit is set (management frame protection, not supported).
    Mgmt { subtype: u8, protected: bool },
    /// A data frame that carries an MSDU or an A-MSDU.
    Data { protected: bool },
    /// Null data or QoS null: no payload (power save, keep-alive).
    NullData,
    /// A control frame (the radio handles ACKs and RTS/CTS itself).
    Control,
    /// Too short for a header, version not 0, or the reserved type: ignore.
    Ignore,
}

/// Classify `frame` (from the MAC header, no FCS). Management and data
/// frames need their 24-octet header; nothing else is read.
pub fn classify(frame: &[u8]) -> FrameClass {
    let (Some(&fc0), Some(&flags)) = (frame.first(), frame.get(1)) else {
        return FrameClass::Ignore;
    };
    if fc0 & 0x03 != 0 {
        return FrameClass::Ignore;
    }
    let protected = flags & fc::PROTECTED != 0;
    let subtype = fc0 >> 4;
    match (fc0 >> 2) & 0x03 {
        TYPE_CONTROL => FrameClass::Control,
        _ if frame.len() < 24 => FrameClass::Ignore,
        TYPE_MGMT => FrameClass::Mgmt { subtype, protected },
        TYPE_DATA if subtype & 0x04 != 0 => FrameClass::NullData,
        TYPE_DATA => FrameClass::Data { protected },
        _ => FrameClass::Ignore,
    }
}

/// The class of a decapsulated MSDU: the 4-way handshake goes to the station
/// manager, everything else to the network stack.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MsduClass {
    Eapol,
    Other,
}

/// Classify an Ethernet frame by its EtherType (EAPOL is 0x888E).
pub fn classify_msdu(eth: &[u8]) -> MsduClass {
    if crate::data::is_eapol(eth) {
        MsduClass::Eapol
    } else {
        MsduClass::Other
    }
}
