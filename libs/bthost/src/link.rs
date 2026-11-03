//! One ACL link and what the host knows about it.

use alloc::vec::Vec;

use bthci::{BdAddr, Boundary};
use bthid::decode::Decoder;
use btsdp::Client;

use crate::le_link::LeLink;

/// Links the host tracks (the controller and `btl2cap` bound it too).
pub const MAX_LINKS: usize = 8;

/// A link that has not become a typing device within this long is dropped:
/// a stranger that connects and stalls must not hold a slot.
pub const SETUP_MS: u64 = 15_000;
/// A link the peer accepted from us but that never authenticates itself gets
/// our `Authentication_Requested` after this long.
pub const AUTH_GRACE_MS: u64 = 1_000;
/// After authentication the peer is given this long to turn encryption on
/// before the host does.
pub const ENCRYPT_GRACE_MS: u64 = 500;
/// A peer that started authenticating (and so reported nothing) is asked to
/// encrypt after this long if it has not.
pub const ENCRYPT_FALLBACK_MS: u64 = 2_000;
/// A peer that is expected to open the HID channels itself gets this long
/// before the host opens them.
pub const OPEN_GRACE_MS: u64 = 1_500;
/// HCI events and ACL data travel on different USB endpoints and no order is
/// promised between them, so a peer's first L2CAP request can reach the host
/// before the `Encryption_Change` that makes it admissible. Data from a link
/// whose key was offered but that is not yet encrypted waits this long (and
/// at most [`MAX_DEFERRED`] fragments, [`MAX_DEFERRED_BYTES`] octets) for it;
/// when it does not come the data is processed as it is, and refused.
pub const DEFER_MS: u64 = 500;
pub const MAX_DEFERRED: usize = 16;
pub const MAX_DEFERRED_BYTES: usize = 2_048;

/// The shortest encryption key a HID link may use. A peer can negotiate down
/// to one octet (CVE-2019-9506, KNOB); a keyboard's keystrokes and a mouse's
/// clicks need the full SSP strength.
pub const MIN_HID_KEY_SIZE: u8 = 16;

/// The live HID state of a link once its sources are registered.
pub(crate) struct Hid {
    pub decoder: Decoder,
}

pub(crate) struct Link {
    pub handle: u16,
    pub addr: BdAddr,
    /// We paged the device (a reconnect it waits for, or a pairing target).
    pub initiated: bool,
    /// Milliseconds on the host clock when the link came up.
    pub up_at: u64,
    /// This link may pair: the window is open and the address is admitted.
    pub pairing: bool,
    /// A bond already existed when the link came up (a reconnect).
    pub reconnect: bool,
    /// A bond was made on this link and is not complete (no SDP record yet).
    pub fresh_bond: bool,
    /// The stored key was refused on a pairing-allowed link; it is gone.
    pub key_retried: bool,
    pub peer_io: Option<u8>,
    /// The passkey the host is showing (B5 shows it; harness builds trace it).
    pub passkey: Option<u32>,
    /// Authentication completed with the key in the bond store.
    pub authenticated: bool,
    pub encrypted: bool,
    /// BR/EDR: encryption is on but the controller has not yet said how many
    /// octets of key it uses (KNOB), so the HID PSMs stay closed.
    pub key_size_pending: bool,
    /// A repeat `Encryption_Change` asked for the key size again on a link
    /// that is already up: a short answer drops it, a good one changes nothing.
    pub key_size_recheck: bool,
    /// The host answered the peer's link key request with the bond's key.
    pub key_offered: bool,
    /// ACL fragments held until the link is encrypted (see [`DEFER_MS`]).
    pub deferred: Vec<(Boundary, Vec<u8>)>,
    pub deferred_bytes: usize,
    pub defer_until: Option<u64>,
    pub sdp_cid: Option<u16>,
    pub control: Option<u16>,
    pub interrupt: Option<u16>,
    pub sdp: Option<Client>,
    pub hid: Option<Hid>,
    /// The first report has been published (for `BT:RECONNECT`).
    pub reported: bool,
    /// A disconnect is on its way: do not ask again.
    pub closing: bool,
    pub setup_deadline: u64,
    pub auth_at: Option<u64>,
    pub encrypt_at: Option<u64>,
    pub open_at: Option<u64>,
    /// The LE half of the link, for an LE link.
    pub le: Option<alloc::boxed::Box<LeLink>>,
}

impl Link {
    pub fn new(handle: u16, addr: BdAddr, now: u64) -> Link {
        Link {
            handle,
            addr,
            initiated: false,
            up_at: now,
            pairing: false,
            reconnect: false,
            fresh_bond: false,
            key_retried: false,
            peer_io: None,
            passkey: None,
            authenticated: false,
            encrypted: false,
            key_size_pending: false,
            key_size_recheck: false,
            key_offered: false,
            deferred: Vec::new(),
            deferred_bytes: 0,
            defer_until: None,
            sdp_cid: None,
            control: None,
            interrupt: None,
            sdp: None,
            hid: None,
            reported: false,
            closing: false,
            setup_deadline: now.saturating_add(SETUP_MS),
            auth_at: None,
            encrypt_at: None,
            open_at: None,
            le: None,
        }
    }

    /// The earliest time something about this link needs the host.
    pub fn deadline(&self) -> Option<u64> {
        let setup = (self.hid.is_none() && !self.closing).then_some(self.setup_deadline);
        [
            setup,
            self.auth_at,
            self.encrypt_at,
            self.open_at,
            // While the key size is pending the setup deadline bounds the
            // wait; a due replay time would only spin the host.
            self.defer_until.filter(|_| !self.key_size_pending),
            self.le.as_ref().and_then(|le| le.deadline()),
        ]
        .into_iter()
        .flatten()
        .min()
    }
}
