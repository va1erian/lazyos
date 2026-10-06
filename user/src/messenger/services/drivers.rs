//! `init`'s `StartDriver` (issue #497): `devd` asks for a driver row to be
//! started for the device it matched. Both halves, over the generated
//! `os.lazy.init.v1` stubs (`idl/init.midl`).

use alloc::string::String;

use libmessenger::Parcel;

use crate::messenger::{Endpoint, Error, Result};

use super::header;
use super::init::{error_field, wire, INTERFACE};

/// What `init` answered: whether it started the row now (false: it was
/// already running for that device), and the driver's task.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Started {
    pub started: bool,
    pub pid: u64,
}

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

/// Call `init`'s `StartDriver`; a supervisor refusal is [`Error::Init`] with
/// its errno.
pub fn start_driver(endpoint: &Endpoint, driver: &str, device: u64) -> Result<Started> {
    let body = wire::encode_start_driver_args(&wire::StartDriverArgs {
        driver: String::from(driver),
        device,
    })
    .map_err(Error::Parcel)?;
    let request = Parcel {
        header: header(INTERFACE, wire::METHOD_STARTDRIVER),
        body,
        ..Parcel::default()
    };
    let reply = endpoint.call(&request, None)?;
    if let Some(code) = error_field(&reply)? {
        return Err(Error::Init(code));
    }
    let reply = wire::decode_start_driver_reply(&reply.body).map_err(Error::Parcel)?;
    Ok(Started {
        started: reply.started,
        pid: reply.pid,
    })
}
