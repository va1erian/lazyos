//! Request dispatch for `os.lazy.mount.v1`. Every decision is
//! `libs/mounttable`'s; this file decodes, spawns and encodes.

use alloc::string::String;
use alloc::vec::Vec;
use user::trace;

use libmessenger::server::{Caller, Fault};
use mounttable::{daemon_args, daemon_env, validate_kind, Kind, Table};
use user::messenger::mount::wire;
use user::messenger::{errno, Error, Result as MResult};
use user::sys::{self, Personality, SpawnCred};

use crate::volumes::Automount;

/// The service's state, answering requests (the generated [`wire::Server`]).
pub(super) struct Handler<'a> {
    pub(super) table: &'a mut Table,
    pub(super) automount: &'a mut Automount,
}

impl wire::Server for Handler<'_> {
    fn mount(
        &mut self,
        cx: &Caller,
        name: String,
        host: String,
        port: u32,
        user: String,
        password: String,
        kind: String,
        share: String,
    ) -> Result<String, Fault> {
        let args = wire::MountArgs {
            name,
            host,
            port,
            user,
            password,
            kind,
            share,
        };
        Ok(mount(self.table, cx, &args)?)
    }

    fn unmount(&mut self, cx: &Caller, name: String) -> Result<(), Fault> {
        Ok(unmount(self.table, cx, &name)?)
    }

    fn list(&mut self, _: &Caller) -> Result<Vec<wire::MountInfo>, Fault> {
        Ok(self.table.entries().iter().map(info).collect())
    }

    fn volumes(&mut self, cx: &Caller) -> Result<Vec<wire::VolumeInfo>, Fault> {
        Ok(self.automount.listing(cx.uid))
    }

    fn eject(&mut self, cx: &Caller, disk: String, force: bool) -> Result<wire::EjectReply, Fault> {
        let disk =
            mounttable::volumes::parse_disk_name(&disk).ok_or(Error::Errno(-errno::EINVAL))?;
        let done = self.automount.eject(disk, cx.uid, force)?;
        Ok(wire::EjectReply {
            state: String::from(done.state),
            detail: done.detail,
        })
    }
}

/// Check the request, start its daemon as the requester's mount, and record
/// it as connecting. The owner is the kernel-stamped caller, never a field
/// of the request.
fn mount(table: &mut Table, caller: &Caller, args: &wire::MountArgs) -> MResult<String> {
    let request = validate_kind(
        &args.kind,
        &args.name,
        &args.host,
        args.port,
        &args.share,
        &args.user,
        &args.password,
    )
    .map_err(|reason| {
        trace::line!(trace::Channel::Mountd, "REFUSED {reason}");
        Error::Errno(-errno::EINVAL)
    })?;
    table.admit(&request).map_err(table_error)?;
    let program = match request.kind {
        Kind::Ftp => fhs::bin::FTPFUSE,
        Kind::Smb => fhs::bin::SMBFUSE,
    };
    let argv = daemon_args(program, &request, caller.uid, caller.gid);
    let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
    // An SMB password travels in the environment, never in `argv`.
    let envp = daemon_env(&request);
    let envp: Vec<&str> = envp.iter().map(String::as_str).collect();
    // Inherit: the daemon gets this service's identity, `CAP_FS_PROVIDER`
    // and nothing more, whoever asked.
    let pid = sys::spawnv(
        program,
        &argv,
        &envp,
        Personality::Native,
        SpawnCred::Inherit,
    )
    .map_err(Error::Errno)?;
    table.add(&request, caller.uid, pid, sys::clock());
    trace::line!(
        trace::Channel::Mountd,
        "START {} kind={} pid={pid}",
        request.name,
        request.kind.name()
    );
    Ok(crate::mount_point(&request.name))
}

fn unmount(table: &mut Table, caller: &Caller, name: &str) -> MResult<()> {
    let daemon = table.remove(name, caller.uid).map_err(table_error)?;
    if let Some(pid) = daemon {
        // Its exit is reaped later and matches no mount any more.
        let _ = sys::kill(pid, sys::SIG_TERM);
    }
    trace::line!(trace::Channel::Mountd, "STOP {name}");
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
        kind: String::from(entry.kind.name()),
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
        share: entry.share.clone(),
    }
}
