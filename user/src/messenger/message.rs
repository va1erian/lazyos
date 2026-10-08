//! A received [`Message`], and the caller credentials the kernel stamps on it.
//!
//! Every message carries a snapshot of its sender's credentials (`uid`,
//! `gid`, capability bits, label and session) taken by the kernel when the
//! message was *queued* (issue #446). [`Message::caller`] is how a service
//! authorizes a request: it needs no `CAP_SETUID` (which reading another
//! task's credentials through `cred_get` does) and cannot be fooled by a
//! sender that exited and whose task slot was reused before the message was
//! read. Never authorize from [`Message::sender`]: it is a slot, not an
//! identity.

use libmessenger::Parcel;

use lazyos_sys::msg::SenderId;

use crate::sys::Cred;

/// `recv` flag: also write the sender's stamped credentials to `parcel_ptr`.
pub(super) use lazyos_sys::msg::op::RECV_SENDER_ID;

/// Bytes of the stamped block (the kernel's `channels::SenderId::SIZE`).
pub(super) const CALLER_BLOCK: usize = SenderId::SIZE;

/// Decode the kernel's stamped block. `None` if a word is out of range for
/// its field, which the kernel never writes: the caller refuses the message
/// rather than guess an identity.
pub(super) fn decode_caller(block: &[u8; CALLER_BLOCK]) -> Option<Cred> {
    SenderId::from_bytes(block).map(SenderId::cred)
}

/// A received message: the decoded parcel plus the kernel-stamped metadata
/// userspace cannot otherwise see.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Message {
    /// Task slot that sent the message. Only for addressing (e.g. a reply
    /// path or a per-client table); authorize with [`Message::caller`].
    pub sender: u64,
    /// The sender's credentials when the message was queued.
    pub caller: Cred,
    /// Kernel transaction id for a call; `None` for one-way messages.
    pub txn: Option<u64>,
    /// The decoded parcel.
    pub parcel: Parcel,
    /// First transferred handle installed by the delivery, as a number in this
    /// task's table. Handle `0` is a valid number, so read [`Message::handles`]
    /// to tell "none" from a real handle. The display protocol reads a client's
    /// event endpoint here.
    pub first_handle: u64,
    /// Number of handles the delivery installed (`0` = the message transferred
    /// none).
    pub handles: u64,
    /// First shared-buffer handle installed by the delivery, ready for
    /// `crate::sys::buffer_map`. [`Message::buffers`] says whether it
    /// is real. The display protocol reads a client's surface buffer here.
    pub first_buffer: u64,
    /// Number of shared-buffer handles the delivery installed.
    pub buffers: u64,
}

impl Message {
    /// The sender's credentials, as the kernel stamped them at queue time.
    pub fn caller(&self) -> Cred {
        self.caller
    }

    /// Method id from the parcel header.
    pub fn method(&self) -> u32 {
        self.parcel.header.method
    }

    /// Interface id from the parcel header.
    pub fn interface_id(&self) -> u64 {
        self.parcel.header.interface_id
    }

    /// Whether the delivery carries exactly the handles and buffers its
    /// method declares in `.midl` (`transfers (...)`), e.g.
    /// `message.carries(wire::OPEN_TRANSFERS)`.
    pub fn carries(&self, declared: messenger_generated::transfers::Transfers) -> bool {
        declared.matches(self.handles, self.buffers)
    }
}
