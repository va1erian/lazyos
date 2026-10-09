//! `mountd` (`/system/bin/mountd`): the network mount service
//! (docs/smb-plan.md §3.4), `os.lazy.mount.v1` (`idl/mount.midl`).
//!
//! Registering a user-space filesystem needs `CAP_FS_PROVIDER`, which an
//! installed app never holds. `mountd` does, and nothing else: it runs as the
//! `_mountd` system user (`mounttable::MOUNTD_UID`) and starts one `ftpfuse`
//! (an FTP server) or `smbfuse` (an SMB share) per `Mount`, which inherits
//! that credential and serves `/mnt/<name>` with its files reported as the
//! requester's (`owner=`). The Network Drives app is its desktop front end.
//!
//! A daemon has no pipe back (native programs have none), so the service
//! learns a mount's fate from the outside: the mount point appearing makes it
//! `mounted`, the daemon's exit makes it `failed` with the reason its exit
//! code names, and `mounttable::MOUNT_TICKS` without either stops it as
//! failed. The rules are `libs/mounttable`, host-tested.
//!
//! Serves `os.lazy.lifecycle.v1`: an orderly shutdown stops every daemon. A
//! crash does not (they are this task's children, not `init`'s): after a
//! restart their mounts keep working but are no longer listed.
//!
//! Serial lines: `MOUNTD:READY`, `MOUNTD:START <name> kind=<kind> pid=<pid>`,
//! `MOUNTD:UP <name>`, `MOUNTD:FAIL <name> <reason>`, `MOUNTD:STOP <name>`.

#![no_std]
#![no_main]

extern crate alloc;

#[path = "mountd/handler.rs"]
mod handler;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::panic::PanicInfo;

use mounttable::Table;
use user::messenger::mount as api;
use user::messenger::services::{self, lifecycle};
use user::messenger::{self, errno, registry, wait, Error};
use user::sys;

/// Ticks between looks for the mount point of a mount still connecting.
const POLL_TICKS: u64 = 20;

#[no_mangle]
pub extern "C" fn _start() -> ! {
    if let Err(error) = run() {
        sys::write_str(&format!("mountd: fatal: {}\n", error.message()));
        sys::exit(1);
    }
    sys::exit(0)
}

fn run() -> messenger::Result<()> {
    let (published, server) = messenger::create_pair()?;
    registry::register(
        api::NAME,
        &published,
        &[api::INTERFACE, lifecycle::INTERFACE],
        0,
    )?;
    sys::write_str(&format!("MOUNTD:READY interface={:#x}\n", api::INTERFACE));
    services::init::notify_ready();

    let mut table = Table::new();
    // One receive buffer for the life of the service.
    let mut buffer = alloc::vec![0u8; messenger::DEFAULT_BUFFER];
    loop {
        housekeeping(&mut table);
        let polling = table.connecting().next().is_some();
        let deadline = polling.then(|| sys::clock() + POLL_TICKS);
        let ready = match wait::wait_any(&[server], wait::WAIT_CHILD, deadline) {
            Ok(ready) => ready,
            Err(Error::Errno(code)) if code == -errno::ETIMEDOUT => 0,
            Err(error) => return Err(error),
        };
        if ready & wait::CHILD_READY != 0 {
            reap(&mut table);
        }
        if ready & 1 == 0 {
            continue;
        }
        let message = match server.recv_with(&mut buffer, Some(messenger::EXPIRED_DEADLINE)) {
            Ok(message) => message,
            Err(Error::Errno(code)) if code == -errno::ETIMEDOUT => continue,
            Err(error) => return Err(error),
        };
        if let Some(reason) = lifecycle::stop_requested(&message) {
            stop_all(&table, &reason);
            return Ok(());
        }
        let reply = handler::dispatch(&mut table, &message).unwrap_or_else(|error| {
            services::error_reply(message.interface_id(), message.method(), error)
        });
        if let Some(txn) = message.txn {
            // A caller whose deadline passed is a normal race, not a failure.
            let _ = server.reply(txn, &reply);
        }
    }
}

/// Promote mounts whose mount point appeared, and stop the ones that took
/// too long.
fn housekeeping(table: &mut Table) {
    let up: Vec<String> = table
        .connecting()
        .filter(|name| user::files::stat(&mount_point(name)).is_ok())
        .map(String::from)
        .collect();
    for name in up {
        table.mounted(&name);
        sys::write_str(&format!("MOUNTD:UP {name}\n"));
    }
    for (name, pid) in table.expire(sys::clock()) {
        let _ = sys::kill(pid, sys::SIG_KILL);
        sys::write_str(&format!("MOUNTD:FAIL {name} {}\n", mounttable::TIMED_OUT));
    }
}

/// Reap every finished child; a daemon of ours fails its mount.
fn reap(table: &mut Table) {
    while let Some((pid, status)) = sys::wait(sys::clock().max(1)) {
        if let Some(entry) = table.exited(pid, status) {
            let reason = entry.state.detail();
            sys::write_str(&format!("MOUNTD:FAIL {} {reason}\n", entry.name));
        }
    }
}

/// The lifecycle stop: end every daemon. Their mounts then fail every call
/// until the kernel takes them out, which is moments before power-off.
fn stop_all(table: &Table, reason: &str) {
    for pid in table.daemons() {
        let _ = sys::kill(pid, sys::SIG_TERM);
    }
    sys::write_str(&format!("MOUNTD:STOP reason=\"{reason}\"\n"));
}

/// `/mnt/<name>`.
pub(crate) fn mount_point(name: &str) -> String {
    format!("{}/{}", fhs::mount::MNT, name)
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    sys::write_str("MOUNTD:FAIL panic\n");
    sys::exit(1)
}
