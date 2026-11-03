//! What the controller model schedules on its timeline.

use bthci::{BdAddr, Event};

pub(crate) enum Task {
    /// The Intel bootloader's verdict, if the stream stayed quiet since.
    RadioVerdict {
        generation: u32,
    },
    Emit(Event),
    /// An event of the inquiry procedure; dropped when it is cancelled.
    Inquiry(Event),
    EmitAcl(Vec<u8>),
    Connected {
        peer: usize,
        peer_initiated: bool,
    },
    ConnectFailed {
        addr: BdAddr,
        status: u8,
    },
    PeerPage {
        peer: usize,
    },
    AcceptTimeout {
        peer: usize,
    },
    AuthStart {
        handle: u16,
        by_peer: bool,
    },
    PasskeyTyped {
        handle: u16,
    },
    /// `status` 0 is success; `key_type` is the HCI link key type then.
    PairingResult {
        handle: u16,
        status: u8,
        key_type: u8,
    },
    Encrypt {
        handle: u16,
        on: bool,
    },
    Disconnected {
        handle: u16,
        reason: u8,
    },
    AclDone {
        handle: u16,
    },
    PeerChannels {
        handle: u16,
    },
    /// The LE keyboard's owner types the passkey that was shown.
    LeTypePasskey {
        shown: u32,
    },
    /// One advertising event of an LE peer.
    LeAdv {
        peer: usize,
    },
    /// The link layer connects the host to an advertising LE peer.
    LeConnected {
        peer: usize,
        address_type: u8,
        addr: BdAddr,
    },
    /// The peer's greeting after the link came up.
    LeGreet {
        handle: u16,
    },
    /// The encryption procedure the host started with `LE_Start_Encryption`.
    LeEncrypt {
        handle: u16,
        ltk: [u8; 16],
    },
}
