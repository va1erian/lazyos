//! Device addresses.

use core::fmt;

/// A 48-bit device address in **wire order** (least significant octet first),
/// so it can be copied to and from a packet unchanged.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct BdAddr(pub [u8; 6]);

impl BdAddr {
    /// From the printed order (`AA:BB:CC:DD:EE:FF` is `[0xAA, .., 0xFF]`).
    pub const fn from_msb(b: [u8; 6]) -> BdAddr {
        BdAddr([b[5], b[4], b[3], b[2], b[1], b[0]])
    }

    /// The printed order.
    pub const fn to_msb(self) -> [u8; 6] {
        let b = self.0;
        [b[5], b[4], b[3], b[2], b[1], b[0]]
    }

    /// A value that prints only the last two octets, the form a default-build
    /// trace may carry (`docs/bluetooth-plan.md`, decision 9).
    pub fn redacted(self) -> Redacted {
        Redacted(self)
    }
}

impl fmt::Display for BdAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let b = self.to_msb();
        write!(
            f,
            "{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
            b[0], b[1], b[2], b[3], b[4], b[5]
        )
    }
}

/// Debug prints the redacted form so a stray `{:?}` never leaks a full address.
impl fmt::Debug for BdAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.redacted(), f)
    }
}

pub struct Redacted(BdAddr);

impl fmt::Display for Redacted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let b = self.0.to_msb();
        write!(f, "xx:xx:xx:xx:{:02X}:{:02X}", b[4], b[5])
    }
}
