//! A deterministic Bluetooth controller model with HID peers
//! (`docs/bluetooth-plan.md` B1).
//!
//! [`SimController`] implements the one [`Controller`] trait the host stack is
//! driven through: HCI commands and ACL data go in, events and ACL data come
//! out, all as functions of the time the caller passes (no clock, no threads,
//! no randomness beyond a seeded generator). The same model backs host tests
//! of `btd`'s libraries and, later, the usbredir fake dongle of the QEMU
//! harness.
//!
//! What it models:
//!
//! * the **controller**: command credits, the event mask (an event the host
//!   did not unmask is not delivered), ACL buffers, scan enable, Secure Simple
//!   Pairing mode, page scan, the LE accept list;
//! * **BR/EDR HID peers** ([`BrPeer`]): a keyboard that pairs with Passkey
//!   Entry and a mouse that pairs with Just Works, each with its own L2CAP
//!   ([`btl2cap`]), an SDP server returning a HID record, and HIDP. They bond,
//!   disconnect, and reconnect (paging the host when the user touches them,
//!   or waiting to be paged), and refuse HID channels on an unencrypted link;
//! * the **link manager** between them: authentication with the stored link
//!   key, SSP, encryption.
//!
//! The model is strict on purpose: a host that sends a command without a
//! credit, overruns the ACL buffers, or sends a malformed packet is recorded
//! in [`SimController::violations`] for the test to assert on.
//!
//! LE peers are not built yet. [`Peer`] is the enum they join (an LE arm, an
//! advertising schedule and an SMP/ATT stack on top of the same `Conn` table),
//! and the LE commands the init sequence sends are already answered.
//!
//! This is a host-only crate and uses `std`.

mod api;
mod cmds;
mod ctrl;
mod gatt;
pub mod hid;
mod lelink;
mod lepeer;
mod link;
pub mod peer;
mod radio;
pub mod sdp;
mod smp_peer;
mod task;

pub use btsmp::pdu::IoCapability;
pub use ctrl::{Peer, PeerId, SimConfig, SimController, Violation};
pub use gatt::GattServer;
pub use hid::HidKind;
pub use lepeer::LePeer;
pub use peer::{BrPeer, PasskeyBehavior, ReconnectStyle};
pub use radio::{Part, RadioFaults, RadioStats};

/// A packet from the controller to the host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HciIn {
    /// An HCI event packet (`code, length, parameters`).
    Event(Vec<u8>),
    /// An HCI ACL data packet.
    Acl(Vec<u8>),
}

/// The seam between the host stack and a controller, real or simulated.
pub trait Controller {
    /// An HCI command packet (`opcode, length, parameters`).
    fn command(&mut self, now_ms: u64, packet: &[u8]);
    /// An HCI ACL data packet to send over the air.
    fn acl_out(&mut self, now_ms: u64, packet: &[u8]);
    /// The next packet the controller sends the host, if any is due by now.
    fn poll_in(&mut self, now_ms: u64) -> Option<HciIn>;
    /// The controller dropped off the USB bus (an Intel recovery reset): how
    /// long it stays off before it comes back as a new device. The transport
    /// disconnects and reconnects; the controller is already in the state it
    /// has after the reconnect. Plain controllers never do.
    fn take_detach(&mut self, _now_ms: u64) -> Option<u64> {
        None
    }
}
