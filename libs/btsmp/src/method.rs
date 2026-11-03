//! Choosing the association model (Core Vol 3, Part H, Table 2.8, LE Secure
//! Connections column) for a host that is always DisplayYesNo and always
//! asks for MITM protection.

use crate::pdu::IoCapability;

/// How the two devices authenticate each other.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Method {
    /// Nobody can compare anything: unauthenticated, resists only passive
    /// eavesdropping.
    JustWorks,
    /// Both devices show six digits and the user confirms they match.
    NumericComparison,
    /// The host shows six digits and the peer keyboard types them.
    PasskeyEntry,
}

impl Method {
    /// The model for a DisplayYesNo initiator and a peer with `peer`.
    ///
    /// A peer that can only display cannot type what we show and we cannot
    /// type what it shows (no keyboard), so the table gives Just Works.
    pub fn select(peer: IoCapability) -> Method {
        match peer {
            IoCapability::KeyboardOnly => Method::PasskeyEntry,
            IoCapability::DisplayYesNo | IoCapability::KeyboardDisplay => Method::NumericComparison,
            IoCapability::DisplayOnly | IoCapability::NoInputNoOutput => Method::JustWorks,
        }
    }

    /// Whether the resulting key counts as authenticated (MITM protected).
    pub fn authenticated(self) -> bool {
        self != Method::JustWorks
    }
}
