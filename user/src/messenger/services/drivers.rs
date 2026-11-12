//! `init`'s `StartDriver` (issue #497): `devd` asks for a driver row to be
//! started for the device it matched. The client is the generated
//! `init::Client::start_driver`; this module is the supervisor's reply.

use libmessenger::Parcel;

use crate::messenger::{Error, Result};

use super::header;
use super::init::{wire, INTERFACE};

/// Encode `init`'s `StartDriver` reply.
pub fn start_driver_reply(started: bool, pid: u64) -> Result<Parcel> {
    let body = wire::encode_start_driver_reply(&wire::StartDriverReply { started, pid })
        .map_err(Error::Parcel)?;
    Ok(Parcel {
        header: header(INTERFACE, wire::METHOD_STARTDRIVER),
        body,
        ..Parcel::default()
    })
}
