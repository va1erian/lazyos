//! `Connect(name)`: a private channel to a registered service (issue #483).
//!
//! [`registry::resolve`] hands every client a handle to the one registered
//! endpoint; [`connect`] instead mints a channel per client
//! ([`channels::connect`]) and posts its service end to the registered
//! endpoint as an `os.lazy.messenger.registry.v1` `Connected` message
//! (`idl/registry.midl`). The service adds the new end to what it serves; a
//! client that closes its end ends its own connection and nobody else's.
//!
//! Only names registered for a channel endpoint can be connected to. The
//! policy gate (`resolve=` rules, labels) is the syscall edge's, exactly as
//! for `Resolve`.

use alloc::vec::Vec;

use libmessenger::{flags, Header, Parcel, VERSION};

use super::channels::{self, Error as ChannelError};
use super::handles::HandleKind;
use super::registry::{self, wire, Error};

/// Open a connection to `name` for the task in `client`; returns the
/// client's handle to its end.
pub fn connect(client: usize, name: &str) -> Result<u64, Error> {
    let (kind, rights, object_id) = registry::lookup(name)?;
    if kind != HandleKind::Channel {
        return Err(Error::BadEndpoint);
    }
    let notice = notice(name)?;
    channels::connect(object_id, client, rights, notice).map_err(|error| match error {
        // The name's endpoint is gone or no longer served.
        ChannelError::InvalidHandle | ChannelError::PeerDied => Error::UnknownName,
        ChannelError::RegistryFull => Error::RegistryFull,
        _ => Error::NoResources,
    })
}

/// The encoded `Connected` notice for `name`. Its handle slot is a
/// placeholder: the kernel installs the real end at delivery.
fn notice(name: &str) -> Result<Vec<u8>, Error> {
    let body = wire::encode_connected_args(&wire::ConnectedArgs { name: name.into() })
        .map_err(|_| Error::BadName)?;
    let parcel = Parcel {
        header: Header {
            version: VERSION,
            flags: flags::ONE_WAY,
            interface_id: registry::INTERFACE,
            method: registry::method::CONNECTED,
            txn_id: 0,
            reply_to: 0,
            deadline_ns: 0,
        },
        body,
        handles: alloc::vec![0],
        buffers: Vec::new(),
    };
    let mut bytes = Vec::new();
    parcel.encode(&mut bytes).map_err(|_| Error::BadName)?;
    Ok(bytes)
}
