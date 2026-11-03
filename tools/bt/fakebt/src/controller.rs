//! The seam between the USB side of the fake dongle and the Bluetooth
//! controller behind it: the trait of `docs/bluetooth-plan.md` ("The
//! simulator"). `libs/btsim` implements the same trait; `fakebt` serves
//! whichever controller `main.rs` builds, so swapping the stub for the
//! simulator is a one-line change there.
//!
//! Packets are HCI packets **without** the H4 indicator: a command is its
//! opcode, length and parameters, an event its code, length and parameters,
//! ACL data its header and data.

/// A packet the controller sends the host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HciIn {
    /// An HCI event: `[code, parameter length, parameters...]`.
    Event(Vec<u8>),
    /// ACL data: `[handle and flags (2), data length (2), data...]`. The stub
    /// never sends any; the simulator's peers do.
    #[allow(dead_code)]
    Acl(Vec<u8>),
}

pub trait Controller {
    /// An HCI command arrived from the host.
    fn command(&mut self, now_ms: u64, packet: &[u8]);
    /// ACL data arrived from the host.
    fn acl_out(&mut self, now_ms: u64, packet: &[u8]);
    /// The next packet the controller sends the host, if any is due by now.
    fn poll_in(&mut self, now_ms: u64) -> Option<HciIn>;
}
