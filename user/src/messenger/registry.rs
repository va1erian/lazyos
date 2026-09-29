//! Service name registry client and daemon protocol (issue #89). See the
//! module doc on [`crate::messenger::registry`] for the two paths into the
//! kernel table.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use libmessenger::{Decoder, Encoder, Header, Kind, Parcel, VERSION};

use super::endpoint::syscall;
use super::{errno, op, Endpoint, Error, MsgArgs, MsgResult, Result, REGISTRY_TARGET_SELF};

/// The bootstrap listener's well-known name. The kernel publishes it at
/// boot; it is the one name every task can resolve.
pub const NAME: &str = "os.lazy.messenger.registry";

/// Registry interface id: the first eight bytes of the spec name
/// `os.lazy.messenger.registry.v1`, mirroring `kernel/src/ipc/registry.rs`.
pub const INTERFACE: u64 = u64::from_le_bytes(*b"os.lazy.");

/// Registry methods, mirroring `kernel/src/ipc/registry.rs`.
pub mod method {
    pub const REGISTER: u32 = 1;
    pub const RESOLVE: u32 = 2;
    pub const UNREGISTER: u32 = 3;
    pub const LIST: u32 = 4;
}

/// Registry TLV field ids, mirroring `kernel/src/ipc/registry.rs`.
pub mod field {
    pub const NAME: u16 = 1;
    pub const INTERFACES: u16 = 2;
    pub const LEASE_TICKS: u16 = 3;
    pub const ENDPOINT: u16 = 4;
    pub const OBJECT: u16 = 5;
    pub const OWNER: u16 = 6;
    pub const LEASE_REMAINING: u16 = 7;
    pub const ENTRY: u16 = 8;
    pub const HANDLE: u16 = 9;
    /// Daemon protocol only: a structured error reply.
    pub const ERROR: u16 = 10;
}

/// Largest `List` reply the client offers the kernel. The table holds at
/// most 64 names of 128 bytes, so 32 KiB has generous room.
pub const LIST_BUFFER: usize = 32 * 1024;

/// One registered name, decoded from a list reply.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Entry {
    /// Service name.
    pub name: String,
    /// Kernel object the name refers to (diagnostic).
    pub object_id: u64,
    /// Task slot that owns the name.
    pub owner_slot: u64,
    /// Interface ids the service implements.
    pub interfaces: Vec<u64>,
    /// Remaining lease ticks; `0` when permanent.
    pub lease_remaining: u64,
}

/// A header for a registry parcel of `method`.
///
/// `ALLOW_NESTED` is required on the shared bootstrap channel: a topic
/// subscriber may be parked in `next_event` (a pending transaction on the
/// same channel) while another task resolves a name, and the kernel's
/// per-channel cycle check would otherwise refuse the resolve.
fn header(method: u32) -> Header {
    Header {
        version: VERSION,
        flags: libmessenger::flags::ALLOW_NESTED,
        interface_id: INTERFACE,
        method,
        txn_id: 0,
        reply_to: 0,
        deadline_ns: 0,
    }
}

/// Wrap an encoded body in a registry parcel.
fn request_parcel(method: u32, body: Encoder) -> Parcel {
    Parcel {
        header: header(method),
        body: body.finish(),
        handles: Vec::new(),
        buffers: Vec::new(),
    }
}

/// A body carrying `name` (the common request shape).
fn name_body(name: &str) -> Result<Encoder> {
    let mut body = Encoder::new();
    body.string(field::NAME, name).map_err(Error::Parcel)?;
    Ok(body)
}

/// A register body: name, endpoint handle, interface array, lease.
fn register_body(
    name: &str,
    endpoint: u64,
    interfaces: &[u64],
    lease_ticks: u64,
) -> Result<Encoder> {
    let mut body = name_body(name)?;
    body.u64(field::ENDPOINT, endpoint).map_err(Error::Parcel)?;
    let mut array = Encoder::new();
    for interface in interfaces {
        array
            .u64(field::INTERFACES, *interface)
            .map_err(Error::Parcel)?;
    }
    body.array(field::INTERFACES, &array)
        .map_err(Error::Parcel)?;
    body.u64(field::LEASE_TICKS, lease_ticks)
        .map_err(Error::Parcel)?;
    Ok(body)
}

/// Encode a parcel for the wire.
fn encode(parcel: &Parcel) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    parcel.encode(&mut bytes).map_err(Error::Parcel)?;
    Ok(bytes)
}

/// Run one registry request through the native gate with `target` as the
/// task whose table the operation touches.
fn registry_call(op_code: u64, target: u64, parcel: &Parcel) -> Result<MsgResult> {
    let bytes = encode(parcel)?;
    let args = MsgArgs {
        txn_id: target,
        parcel_ptr: bytes.as_ptr() as u64,
        parcel_len: bytes.len() as u64,
        ..MsgArgs::default()
    };
    let mut result = MsgResult::default();
    syscall(op_code, &args, &mut result)?;
    Ok(result)
}

/// Register `endpoint` in the table on behalf of `target`'s task.
///
/// The direct path passes [`REGISTRY_TARGET_SELF`]; [`serve_request`]
/// passes the requester's slot and the kernel checks the proxy capability.
fn register_for(
    target: u64,
    name: &str,
    endpoint: u64,
    interfaces: &[u64],
    lease_ticks: u64,
) -> Result<()> {
    let parcel = request_parcel(
        method::REGISTER,
        register_body(name, endpoint, interfaces, lease_ticks)?,
    );
    registry_call(op::REGISTER, target, &parcel)?;
    Ok(())
}

/// Publish `endpoint` under `name`; the caller becomes the owner. A
/// `lease_ticks` of `0` registers a permanent name.
pub fn register(
    name: &str,
    endpoint: &Endpoint,
    interfaces: &[u64],
    lease_ticks: u64,
) -> Result<()> {
    register_for(
        REGISTRY_TARGET_SELF,
        name,
        endpoint.handle(),
        interfaces,
        lease_ticks,
    )
}

/// Resolve `name` into `target`'s table and return the new handle.
fn resolve_for(target: u64, name: &str) -> Result<Endpoint> {
    let parcel = request_parcel(method::RESOLVE, name_body(name)?);
    let result = registry_call(op::RESOLVE, target, &parcel)?;
    Ok(Endpoint::from_raw(result.value))
}

/// Resolve `name`; the returned endpoint is open in this task's table.
pub fn resolve(name: &str) -> Result<Endpoint> {
    resolve_for(REGISTRY_TARGET_SELF, name)
}

/// Withdraw `name` on behalf of `target`'s task.
fn unregister_for(target: u64, name: &str) -> Result<()> {
    let parcel = request_parcel(method::UNREGISTER, name_body(name)?);
    registry_call(op::UNREGISTER, target, &parcel)?;
    Ok(())
}

/// Withdraw `name`. Only its owner (or an administrator) may.
pub fn unregister(name: &str) -> Result<()> {
    unregister_for(REGISTRY_TARGET_SELF, name)
}

/// Snapshot the name table.
pub fn list() -> Result<Vec<Entry>> {
    let mut buf = vec![0u8; LIST_BUFFER];
    let args = MsgArgs {
        buf_ptr: buf.as_mut_ptr() as u64,
        buf_cap: buf.len() as u64,
        ..MsgArgs::default()
    };
    let mut result = MsgResult::default();
    syscall(op::LIST, &args, &mut result)?;
    let len = result.bytes as usize;
    if len > buf.len() {
        return Err(Error::Errno(-errno::E2BIG));
    }
    let parcel = Parcel::decode(&buf[..len]).map_err(Error::Parcel)?;
    decode_entries(&parcel)
}

/// The first string field with `id`.
fn string_field(parcel: &Parcel, id: u16) -> Result<String> {
    let mut decoder = Decoder::new(&parcel.body);
    while let Some(field) = decoder.next().map_err(Error::Parcel)? {
        if field.kind == Kind::String && field.id == id {
            return Ok(String::from(field.as_str().map_err(Error::Parcel)?));
        }
    }
    Err(Error::Errno(-errno::EINVAL))
}

/// The first `u64` field with `id`, if any.
fn u64_field(parcel: &Parcel, id: u16) -> Result<Option<u64>> {
    let mut decoder = Decoder::new(&parcel.body);
    while let Some(field) = decoder.next().map_err(Error::Parcel)? {
        if field.kind == Kind::U64 && field.id == id {
            return Ok(Some(field.as_u64().map_err(Error::Parcel)?));
        }
    }
    Ok(None)
}

/// Decode the interface id array.
fn interfaces_field(parcel: &Parcel) -> Result<Vec<u64>> {
    let mut decoder = Decoder::new(&parcel.body);
    while let Some(field) = decoder.next().map_err(Error::Parcel)? {
        if field.kind == Kind::Array && field.id == field::INTERFACES {
            let mut nested = field.nested(0).map_err(Error::Parcel)?;
            let mut interfaces = Vec::new();
            while let Some(item) = nested.next().map_err(Error::Parcel)? {
                if item.kind == Kind::U64 {
                    interfaces.push(item.as_u64().map_err(Error::Parcel)?);
                }
            }
            return Ok(interfaces);
        }
    }
    Ok(Vec::new())
}

/// Decode a list reply body into entries; unknown fields are skipped so a
/// newer kernel stays compatible with this client.
fn decode_entries(parcel: &Parcel) -> Result<Vec<Entry>> {
    let mut entries = Vec::new();
    let mut decoder = Decoder::new(&parcel.body);
    while let Some(record) = decoder.next().map_err(Error::Parcel)? {
        if record.kind != Kind::Struct || record.id != field::ENTRY {
            continue;
        }
        let mut nested = record.nested(0).map_err(Error::Parcel)?;
        let mut entry = Entry {
            name: String::new(),
            object_id: 0,
            owner_slot: 0,
            interfaces: Vec::new(),
            lease_remaining: 0,
        };
        while let Some(item) = nested.next().map_err(Error::Parcel)? {
            match (item.kind, item.id) {
                (Kind::String, field::NAME) => {
                    entry.name = String::from(item.as_str().map_err(Error::Parcel)?);
                }
                (Kind::U64, field::OBJECT) => {
                    entry.object_id = item.as_u64().map_err(Error::Parcel)?;
                }
                (Kind::U64, field::OWNER) => {
                    entry.owner_slot = item.as_u64().map_err(Error::Parcel)?;
                }
                (Kind::U64, field::LEASE_REMAINING) => {
                    entry.lease_remaining = item.as_u64().map_err(Error::Parcel)?;
                }
                (Kind::Array, field::INTERFACES) => {
                    let mut array = item.nested(0).map_err(Error::Parcel)?;
                    while let Some(id) = array.next().map_err(Error::Parcel)? {
                        if id.kind == Kind::U64 {
                            entry.interfaces.push(id.as_u64().map_err(Error::Parcel)?);
                        }
                    }
                }
                _ => {}
            }
        }
        entries.push(entry);
    }
    Ok(entries)
}

/// Encode entries into a list reply parcel (the daemon's `List` answer and
/// the tests reuse this so both directions share one format).
fn encode_entries(entries: &[Entry]) -> Result<Parcel> {
    let mut body = Encoder::new();
    for entry in entries {
        let mut record = Encoder::new();
        record
            .string(field::NAME, &entry.name)
            .map_err(Error::Parcel)?;
        record
            .u64(field::OBJECT, entry.object_id)
            .map_err(Error::Parcel)?;
        record
            .u64(field::OWNER, entry.owner_slot)
            .map_err(Error::Parcel)?;
        let mut interfaces = Encoder::new();
        for interface in &entry.interfaces {
            interfaces
                .u64(field::INTERFACES, *interface)
                .map_err(Error::Parcel)?;
        }
        record
            .array(field::INTERFACES, &interfaces)
            .map_err(Error::Parcel)?;
        record
            .u64(field::LEASE_REMAINING, entry.lease_remaining)
            .map_err(Error::Parcel)?;
        body.record(field::ENTRY, &record).map_err(Error::Parcel)?;
    }
    Ok(request_parcel(method::LIST, body))
}

/// The daemon's request handler: forward one registry request to the kernel
/// on behalf of `sender` (the kernel-stamped task slot), and build the
/// reply parcel.
///
/// The endpoint handle in a `Register` request is a number in the sender's
/// table; the kernel reads it there, so nothing crosses tables here.
pub fn serve_request(request: &Parcel, sender: u64) -> Result<Parcel> {
    match request.header.method {
        method::REGISTER => {
            let name = string_field(request, field::NAME)?;
            let endpoint =
                u64_field(request, field::ENDPOINT)?.ok_or(Error::Errno(-errno::EINVAL))?;
            let interfaces = interfaces_field(request)?;
            let lease = u64_field(request, field::LEASE_TICKS)?.unwrap_or(0);
            register_for(sender, &name, endpoint, &interfaces, lease)?;
            Ok(request_parcel(method::REGISTER, Encoder::new()))
        }
        method::RESOLVE => {
            let name = string_field(request, field::NAME)?;
            let endpoint = resolve_for(sender, &name)?;
            let mut body = Encoder::new();
            body.u64(field::HANDLE, endpoint.handle())
                .map_err(Error::Parcel)?;
            Ok(request_parcel(method::RESOLVE, body))
        }
        method::UNREGISTER => {
            let name = string_field(request, field::NAME)?;
            unregister_for(sender, &name)?;
            Ok(request_parcel(method::UNREGISTER, Encoder::new()))
        }
        method::LIST => {
            let entries = list()?;
            encode_entries(&entries)
        }
        _ => Err(Error::Errno(-errno::EINVAL)),
    }
}

/// The daemon's error answer: the errno-style code plus the friendly text,
/// so the client can return a [`Error::Registry`] with a readable message.
pub fn error_reply(method: u32, error: Error) -> Parcel {
    let code = error.errno().map(|code| -code).unwrap_or(errno::EINVAL);
    let mut body = Encoder::new();
    // A structured error field cannot overflow a fresh encoder here.
    let _ = body.error(field::ERROR, code as u32, error.message());
    request_parcel(method, body)
}

/// The first structured error field, when the reply is a daemon failure.
fn error_field(parcel: &Parcel) -> Result<Option<i64>> {
    let mut decoder = Decoder::new(&parcel.body);
    while let Some(field) = decoder.next().map_err(Error::Parcel)? {
        if field.kind == Kind::Error && field.id == field::ERROR {
            let (code, _message) = field.error_parts().map_err(Error::Parcel)?;
            return Ok(Some(code as i64));
        }
    }
    Ok(None)
}

/// A client of the `messengerd` daemon over the bootstrap channel.
pub struct Client {
    endpoint: Endpoint,
}

impl Client {
    /// Resolve the well-known registry name and wrap its endpoint.
    pub fn connect() -> Result<Client> {
        Ok(Client {
            endpoint: resolve(NAME)?,
        })
    }

    /// The underlying daemon endpoint (diagnostics).
    pub fn endpoint(&self) -> Endpoint {
        self.endpoint
    }

    /// Run one request as a blocking call and fail on a daemon error reply.
    fn call(&self, method: u32, body: Encoder) -> Result<Parcel> {
        let reply = self.endpoint.call(&request_parcel(method, body), None)?;
        if let Some(code) = error_field(&reply)? {
            return Err(Error::Registry(code));
        }
        Ok(reply)
    }

    /// Register `endpoint` under `name` through the daemon. The daemon
    /// forwards the request with this task as the owner.
    pub fn register(
        &self,
        name: &str,
        endpoint: &Endpoint,
        interfaces: &[u64],
        lease_ticks: u64,
    ) -> Result<()> {
        self.call(
            method::REGISTER,
            register_body(name, endpoint.handle(), interfaces, lease_ticks)?,
        )?;
        Ok(())
    }

    /// Resolve `name` through the daemon; the returned endpoint is open in
    /// this task's table.
    pub fn resolve(&self, name: &str) -> Result<Endpoint> {
        let reply = self.call(method::RESOLVE, name_body(name)?)?;
        let handle = u64_field(&reply, field::HANDLE)?.ok_or(Error::Errno(-errno::EINVAL))?;
        Ok(Endpoint::from_raw(handle))
    }

    /// Withdraw `name` through the daemon.
    pub fn unregister(&self, name: &str) -> Result<()> {
        self.call(method::UNREGISTER, name_body(name)?)?;
        Ok(())
    }

    /// Snapshot the table through the daemon.
    pub fn list(&self) -> Result<Vec<Entry>> {
        let reply = self.call(method::LIST, Encoder::new())?;
        decode_entries(&reply)
    }
}
