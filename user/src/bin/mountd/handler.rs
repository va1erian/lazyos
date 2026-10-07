//! Request dispatch for `os.lazy.mount.v1`. Every decision is
//! `libs/mounttable`'s; this file decodes, spawns and encodes.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use mounttable::{daemon_args, validate, Table, KIND_FTP};
use user::messenger::mount::{self as api, wire};
use user::messenger::{errno, Error, Message, Parcel, Result};
use user::sys::{self, Personality, SpawnCred};

/// Route one inbound message. `Ok(parcel)` is the reply; `Err` becomes the
/// structured error reply the caller sees.
pub(super) fn dispatch(table: &mut Table, message: &Message) -> Result<Parcel> {
    if message.interface_id() != api::INTERFACE {
        return Err(Error::Errno(-errno::EINVAL));
    }
    let method = message.method();
    let body = match method {
        wire::METHOD_MOUNT => {
            let args = wire::decode_mount_args(&message.parcel.body).map_err(Error::Parcel)?;
            let path = mount(table, message, &args)?;
            wire::encode_mount_reply(&wire::MountReply { path }).map_err(Error::Parcel)?
        }
        wire::METHOD_UNMOUNT => {
            let args = wire::decode_unmount_args(&message.parcel.body).map_err(Error::Parcel)?;
            unmount(table, message, &args.name)?;
            Vec::new()
        }
        wire::METHOD_LIST => wire::encode_list_reply(&wire::ListReply {
            mounts: table.entries().iter().map(info).collect(),
        })
        .map_err(Error::Parcel)?,
        _ => return Err(Error::Errno(-errno::EINVAL)),
    };
    Ok(api::parcel(method, body))
}

/// Check the request, start its daemon as the requester's mount, and record
/// it as connecting. The owner is the kernel-stamped caller, never a field
/// of the request.
fn mount(table: &mut Table, message: &Message, args: &wire::MountArgs) -> Result<String> {
    let request = validate(
        &args.name,
        &args.host,
        args.port,
        &args.user,
        &args.password,
    )
    .map_err(|reason| {
        sys::write_str(&format!("MOUNTD:REFUSED {reason}\n"));
        Error::Errno(-errno::EINVAL)
    })?;
    table.admit(&request).map_err(table_error)?;
    let caller = message.caller();
    let argv = daemon_args(fhs::bin::FTPFUSE, &request, caller.uid, caller.gid);
    let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
    // Inherit: the daemon gets this service's identity, `CAP_FS_PROVIDER`
    // and nothing more, whoever asked.
    let pid = sys::spawnv(
        fhs::bin::FTPFUSE,
        &argv,
        &[],
        Personality::Native,
        SpawnCred::Inherit,
    )
    .map_err(Error::Errno)?;
    table.add(&request, caller.uid, pid, sys::clock());
    sys::write_str(&format!("MOUNTD:START {} pid={pid}\n", request.name));
    Ok(crate::mount_point(&request.name))
}

fn unmount(table: &mut Table, message: &Message, name: &str) -> Result<()> {
    let daemon = table
        .remove(name, message.caller().uid)
        .map_err(table_error)?;
    if let Some(pid) = daemon {
        // Its exit is reaped later and matches no mount any more.
        let _ = sys::kill(pid, sys::SIG_TERM);
    }
    sys::write_str(&format!("MOUNTD:STOP {name}\n"));
    Ok(())
}

fn table_error(error: mounttable::Error) -> Error {
    Error::Errno(-match error {
        mounttable::Error::Exists => errno::EEXIST,
        mounttable::Error::Full => errno::EAGAIN,
        mounttable::Error::NotFound => errno::ENOENT,
        mounttable::Error::Denied => errno::EPERM,
    })
}

fn info(entry: &mounttable::Entry) -> wire::MountInfo {
    wire::MountInfo {
        name: entry.name.clone(),
        kind: String::from(KIND_FTP),
        host: entry.host.clone(),
        port: u32::from(entry.port),
        user: if entry.user.is_empty() {
            String::from("anonymous")
        } else {
            entry.user.clone()
        },
        path: crate::mount_point(&entry.name),
        state: String::from(entry.state.name()),
        detail: String::from(entry.state.detail()),
        owner: entry.owner,
    }
}
