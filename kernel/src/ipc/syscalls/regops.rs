//! Name registry ops (issue #89).

use super::*;
use crate::ipc::policy::{self, NameOp};

/// Resolve the task slot a registry op acts on.
///
/// [`REGISTRY_TARGET_SELF`] (and the caller's own slot) mean "me". Any other
/// slot is a privileged proxy request: `messengerd` forwards a client's
/// register/resolve/unregister with the client's slot, which requires
/// `CAP_IPC_CONTROL`. This is how a client gets a handle without ever naming
/// another process's table.
pub(super) fn registry_target(requested: u64) -> Result<usize, i64> {
    let me = task::current();
    if requested == REGISTRY_TARGET_SELF || requested == me as u64 {
        return Ok(me);
    }
    if !credentials::of(me).has_cap(credentials::CAP_IPC_CONTROL) {
        return Err(errno::EPERM);
    }
    let target = usize::try_from(requested).map_err(|_| errno::EINVAL)?;
    if target >= task::MAX_TASKS {
        return Err(errno::EINVAL);
    }
    Ok(target)
}

/// Authorize one registry method and audit the verdict through the shared hook.
pub(super) fn authorize_registry(actor_slot: usize, method: u32) -> Result<(), i64> {
    if crate::ipc::authorize(actor_slot, registry::INTERFACE, method, 0).denied() {
        return Err(errno::EACCES);
    }
    Ok(())
}

/// Registry errors to errno values.
pub(super) fn registry_errno(error: registry::Error) -> i64 {
    use registry::Error::*;
    match error {
        BadName | BadEndpoint | TooManyInterfaces | BadTask | BadLease => errno::EINVAL,
        NameTaken => errno::EEXIST,
        UnknownName => errno::ENOENT,
        NotOwner => errno::EPERM,
        RegistryFull | NoResources => errno::ENOMEM,
    }
}

/// The one op entry for the registry family: authorize, pick the target task,
/// then dispatch by method. Every method takes its inputs from the request
/// parcel, so the same body works for a direct syscall and for `messengerd`
/// forwarding a client's request.
pub(super) fn op_registry(args: &MsgArgs, method: u32) -> Result<MsgResult, i64> {
    authorize_registry(task::current(), method)?;
    let target = registry_target(args.txn_id)?;
    match method {
        registry::method::REGISTER => registry_register(args, target),
        registry::method::RESOLVE => registry_resolve(args, target),
        registry::method::UNREGISTER => registry_unregister(args, target),
        registry::method::LIST => registry_list(args),
        registry::method::CONNECT => registry_connect(args, target),
        _ => Err(errno::EINVAL),
    }
}

/// Decode a registry request parcel and its generated argument body; a
/// malformed parcel or body is `EINVAL`.
fn registry_args<T>(
    bytes: &[u8],
    decode: fn(&[u8]) -> Result<T, libmessenger::Error>,
) -> Result<T, i64> {
    let parcel = decode_parcel(bytes)?;
    decode(parcel.body()).map_err(|_| errno::EINVAL)
}

/// `OP_REGISTER`: publish the endpoint named by the request's `endpoint`
/// argument under `name`, with `interfaces` and an optional `lease_ticks`. The
/// handle is read from `target`'s table, so the recorded owner is that task.
pub(super) fn registry_register(args: &MsgArgs, target: usize) -> Result<MsgResult, i64> {
    let bytes = read_parcel(args)?;
    let args = registry_args(&bytes, registry::wire::decode_register_args)?;
    // Handle `0` is a valid slot, so an absent endpoint must not decode as it.
    let endpoint = args.endpoint.ok_or(errno::EINVAL)?;
    // Name policy first, keyed by the task that will own the name (the client
    // when `messengerd` proxies), so a denied app learns nothing about handles.
    policy::check_name(target, NameOp::Register, &args.name).map_err(|_| errno::EACCES)?;
    // Then what it claims to serve: an app only its own domain (issue #495).
    policy::check_interfaces(target, &args.interfaces, &args.interface_names)
        .map_err(|_| errno::EACCES)?;
    let entry = handles::get_for_task(target, endpoint).map_err(handles_errno)?;
    if !matches!(entry.kind, HandleKind::Channel | HandleKind::Endpoint) {
        return Err(errno::EINVAL);
    }
    // A device interrupt channel is its claimant's alone (issue #496).
    if entry.kind == HandleKind::Channel && crate::ipc::channels::is_irq_channel(entry.object_id) {
        return Err(errno::EINVAL);
    }
    registry::register(
        target,
        &args.name,
        entry.kind,
        entry.rights,
        entry.object_id,
        &args.interfaces,
        args.lease_ticks,
    )
    .map_err(registry_errno)?;
    Ok(MsgResult {
        value: entry.object_id,
        ..MsgResult::default()
    })
}

/// `OP_CONNECT`: open a private connection to `name` for `target` (issue
/// #483). The name policy is `Resolve`'s: a connection reaches exactly what a
/// resolve would.
pub(super) fn registry_connect(args: &MsgArgs, target: usize) -> Result<MsgResult, i64> {
    let bytes = read_parcel(args)?;
    let args = registry_args(&bytes, registry::wire::decode_connect_args)?;
    policy::check_name(target, NameOp::Resolve, &args.name).map_err(|_| errno::EACCES)?;
    let handle = crate::ipc::connect::connect(target, &args.name).map_err(registry_errno)?;
    Ok(MsgResult {
        value: handle,
        ..MsgResult::default()
    })
}

/// `OP_RESOLVE`: look up `name` and open its endpoint in `target`'s table.
pub(super) fn registry_resolve(args: &MsgArgs, target: usize) -> Result<MsgResult, i64> {
    let bytes = read_parcel(args)?;
    let args = registry_args(&bytes, registry::wire::decode_resolve_args)?;
    // Denied before the lookup, so a refusal cannot be used to probe which
    // names exist.
    policy::check_name(target, NameOp::Resolve, &args.name).map_err(|_| errno::EACCES)?;
    let handle = registry::resolve(target, &args.name).map_err(registry_errno)?;
    Ok(MsgResult {
        value: handle,
        ..MsgResult::default()
    })
}

/// `OP_UNREGISTER`: withdraw `name` on behalf of `target`'s task.
pub(super) fn registry_unregister(args: &MsgArgs, target: usize) -> Result<MsgResult, i64> {
    let bytes = read_parcel(args)?;
    let args = registry_args(&bytes, registry::wire::decode_unregister_args)?;
    registry::unregister(task::current(), target, &args.name).map_err(registry_errno)?;
    Ok(MsgResult::default())
}

/// `OP_LIST`: encode the table as a parcel whose body has one `Entry`
/// per name, then copy it into the caller's buffer. The wire shape is shared
/// with `user/src/messenger/`, which always offers a large enough buffer.
pub(super) fn registry_list(args: &MsgArgs) -> Result<MsgResult, i64> {
    let entries = registry::list()
        .into_iter()
        .map(|entry| registry::wire::Entry {
            name: entry.name,
            object: entry.object_id,
            owner: entry.owner_slot as u64,
            interfaces: entry.interfaces,
            lease_remaining: entry.lease_remaining.unwrap_or(0),
        })
        .collect();
    let body = registry::wire::encode_list_reply(&registry::wire::ListReply { entries })
        .map_err(|_| errno::E2BIG)?;
    let parcel = Parcel {
        header: Header {
            version: VERSION,
            flags: 0,
            interface_id: registry::INTERFACE,
            method: registry::method::LIST,
            txn_id: 0,
            reply_to: 0,
            deadline_ns: 0,
        },
        body,
        objects: Vec::new(),
    };
    let mut encoded = Vec::new();
    parcel.encode(&mut encoded).map_err(|_| errno::E2BIG)?;
    if encoded.len() > args.buf_cap as usize {
        return Err(errno::E2BIG);
    }
    copy_out(args.buf_ptr, &encoded)?;
    Ok(MsgResult {
        bytes: encoded.len() as u64,
        ..MsgResult::default()
    })
}
