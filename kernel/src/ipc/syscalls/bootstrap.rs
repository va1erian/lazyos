//! The boot-time Messenger channel (issue #69).
//!
//! `kernel_main` calls [`create`] once: it opens an endpoint pair in the
//! kernel's handle table, keeps the service end for the `messengerd` stub,
//! and records the client end's object id. The first userspace task to
//! call the `bootstrap` op gets a fresh handle to that client end opened
//! in its own table, so the capability transfer happens inside the kernel
//! and the task never needs to name another process's handle.

use spin::Mutex;

use super::{channels, errno, handles, handles_errno, task};
use crate::ipc::handles::HandleKind;

/// Registry entry: the kernel-owned handles and the claim state.
struct Channel {
    /// Kernel handle for the client end, kept so the channel survives
    /// until a task claims it.
    client: u64,
    /// Kernel handle for the service end; the stub serves on this.
    server: u64,
    /// Object id (channel id + side) `client` names; opening a handle with
    /// this id in another task's table aliases the same endpoint.
    client_object: u64,
    /// Object id of the service end. [`publish`] registers it under the
    /// well-known registry name, so resolving the name hands a caller the
    /// side opposite the daemon's, which is where requests arrive.
    server_object: u64,
    /// Whether a task has already taken the client end.
    claimed: bool,
}

static BOOTSTRAP: Mutex<Option<Channel>> = Mutex::new(None);

/// Create the bootstrap pair. Called once from `kernel_main`, in kernel
/// context (the handles open in the kernel task's table).
pub fn create() -> Result<(), &'static str> {
    let (client, server) = channels::create().map_err(|error| error.message())?;
    let client_object = handles::get(client)
        .map_err(|error| error.message())?
        .object_id;
    let server_object = handles::get(server)
        .map_err(|error| error.message())?
        .object_id;
    *BOOTSTRAP.lock() = Some(Channel {
        client,
        server,
        client_object,
        server_object,
        claimed: false,
    });
    Ok(())
}

/// The kernel-held service endpoint, for the `messengerd` stub.
pub fn service_handle() -> Option<u64> {
    BOOTSTRAP.lock().as_ref().map(|channel| channel.server)
}

/// Publish the kernel-held service endpoint under `name` (issue #89).
///
/// `kernel_main` calls this once after [`create`] with
/// `os.lazy.messenger.registry`: any task that resolves the name receives a
/// handle to the service end, and calls on it are delivered to the daemon
/// that claimed the client end. The kernel keeps the handle open, so the
/// name's object stays alive for the life of the system.
pub fn publish(name: &str) -> Result<(), &'static str> {
    let handle = service_handle().ok_or("the bootstrap channel is not ready")?;
    let entry = handles::get(handle).map_err(|error| error.message())?;
    crate::ipc::registry::register(
        task::KERNEL_TASK,
        name,
        entry.kind,
        entry.rights,
        entry.object_id,
        &[crate::ipc::registry::INTERFACE],
        0,
    )
    .map_err(|error| error.message())
}

/// Open the client end in the calling task's handle table.
///
/// Exactly one userspace task may claim it; the kernel task is refused (it
/// already owns the pair). A second claim fails with `-EBUSY` rather than
/// silently handing out a second capability.
pub fn claim_client() -> Result<u64, i64> {
    if task::current() == task::KERNEL_TASK {
        return Err(errno::EPERM);
    }
    let mut guard = BOOTSTRAP.lock();
    let channel = guard.as_mut().ok_or(errno::ENOENT)?;
    if channel.claimed {
        return Err(errno::EBUSY);
    }
    let handle = handles::open(
        HandleKind::Channel,
        handles::rights::ALL,
        channel.client_object,
    )
    .map_err(handles_errno)?;
    channel.claimed = true;
    Ok(handle)
}

/// Serve one queued request by echoing its parcel back.
///
/// A synchronous request is answered with `reply`; a one-way message has
/// no transaction, so the stub sends the same bytes back to the peer — the
/// same echo either way. This is the `messengerd` stub for the #69 slice:
/// enough to prove the bootstrap path end to end, not a name registry.
/// Returns whether a request was handled.
pub fn stub_serve() -> Result<bool, channels::Error> {
    let Some(server) = service_handle() else {
        return Ok(false);
    };
    let Some(message) = channels::try_recv(server)? else {
        return Ok(false);
    };
    match message.txn {
        Some(txn) => channels::reply(txn, &message.bytes)?,
        None => channels::send(server, &message.bytes)?,
    }
    Ok(true)
}

/// Drop the registry and the kernel-held endpoints. Kernel context only
/// (tests and reboot); a running system never tears the bootstrap down.
pub fn reset() {
    if let Some(channel) = BOOTSTRAP.lock().take() {
        handles::close(channel.client).ok();
        handles::close(channel.server).ok();
    }
}
