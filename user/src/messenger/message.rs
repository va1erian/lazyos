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

use crate::sys::Cred;

/// `recv` flag: also write the sender's stamped credentials to `parcel_ptr`
/// (the kernel's `ipc::syscalls::RECV_SENDER_ID`).
pub(super) const RECV_SENDER_ID: u64 = 1;

/// Bytes of the stamped block: `uid, gid, label_id, session, caps` as
/// little-endian `u64` words (the kernel's `channels::SenderId::SIZE`).
pub(super) const CALLER_BLOCK: usize = 40;

/// Decode the kernel's stamped block. `None` if a word is out of range for
/// its field, which the kernel never writes: the caller refuses the message
/// rather than guess an identity.
pub(super) fn decode_caller(block: &[u8; CALLER_BLOCK]) -> Option<Cred> {
    let mut words = [0u64; CALLER_BLOCK / 8];
    for (word, chunk) in words.iter_mut().zip(block.as_chunks::<8>().0) {
        *word = u64::from_le_bytes(*chunk);
    }
    let [uid, gid, label_id, session, caps] = words;
    Some(Cred::new(
        u32::try_from(uid).ok()?,
        u32::try_from(gid).ok()?,
        u32::try_from(caps).ok()?,
        u32::try_from(label_id).ok()?,
        session,
    ))
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
    /// `crate::sys::display_map_buffer`. [`Message::buffers`] says whether it
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
