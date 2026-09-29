//! Name registry ops (issue #89).

use super::*;

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
        _ => Err(errno::EINVAL),
    }
}

/// Find a string field in a registry request body.
pub(super) fn registry_name(parcel: &Parcel) -> Result<alloc::string::String, i64> {
    let mut decoder = Decoder::new(&parcel.body);
    while let Some(field) = decoder.next().map_err(|_| errno::EINVAL)? {
        if field.kind == Kind::String && field.id == registry::field::NAME {
            return Ok(alloc::string::String::from(
                field.as_str().map_err(|_| errno::EINVAL)?,
            ));
        }
    }
    Err(errno::EINVAL)
}

/// Find the interface id array of a registry request body (missing means none).
pub(super) fn registry_interfaces(parcel: &Parcel) -> Result<Vec<u64>, i64> {
    let mut decoder = Decoder::new(&parcel.body);
    while let Some(field) = decoder.next().map_err(|_| errno::EINVAL)? {
        if field.kind == Kind::Array && field.id == registry::field::INTERFACES {
            let mut nested = field.nested(0).map_err(|_| errno::EINVAL)?;
            let mut interfaces = Vec::new();
            while let Some(item) = nested.next().map_err(|_| errno::EINVAL)? {
                if item.kind == Kind::U64 {
                    interfaces.push(item.as_u64().map_err(|_| errno::EINVAL)?);
                }
            }
            return Ok(interfaces);
        }
    }
    Ok(Vec::new())
}

/// Find a `u64` field in a registry request body.
pub(super) fn registry_u64(parcel: &Parcel, id: u16) -> Option<u64> {
    let mut decoder = Decoder::new(&parcel.body);
    while let Ok(Some(field)) = decoder.next() {
        if field.kind == Kind::U64 && field.id == id {
            return field.as_u64().ok();
        }
    }
    None
}

/// `OP_REGISTER`: publish the endpoint named by the request body's `ENDPOINT`
/// field under `NAME`, with `INTERFACES` and an optional `LEASE_TICKS`. The
/// handle is read from `target`'s table, so the recorded owner is that task.
pub(super) fn registry_register(args: &MsgArgs, target: usize) -> Result<MsgResult, i64> {
    let bytes = read_parcel(args)?;
    let parcel = decode_parcel(&bytes)?;
    let handle = registry_u64(&parcel, registry::field::ENDPOINT).ok_or(errno::EINVAL)?;
    let entry = handles::get_for_task(target, handle).map_err(handles_errno)?;
    if !matches!(entry.kind, HandleKind::Channel | HandleKind::Endpoint) {
        return Err(errno::EINVAL);
    }
    let name = registry_name(&parcel)?;
    let interfaces = registry_interfaces(&parcel)?;
    let lease = registry_u64(&parcel, registry::field::LEASE_TICKS).unwrap_or(0);
    registry::register(
        target,
        &name,
        entry.kind,
        entry.rights,
        entry.object_id,
        &interfaces,
        lease,
    )
    .map_err(registry_errno)?;
    Ok(MsgResult {
        value: entry.object_id,
        ..MsgResult::default()
    })
}

/// `OP_RESOLVE`: look up `NAME` and open its endpoint in `target`'s table.
pub(super) fn registry_resolve(args: &MsgArgs, target: usize) -> Result<MsgResult, i64> {
    let bytes = read_parcel(args)?;
    let parcel = decode_parcel(&bytes)?;
    let name = registry_name(&parcel)?;
    let handle = registry::resolve(target, &name).map_err(registry_errno)?;
    Ok(MsgResult {
        value: handle,
        ..MsgResult::default()
    })
}

/// `OP_UNREGISTER`: withdraw `NAME` on behalf of `target`'s task.
pub(super) fn registry_unregister(args: &MsgArgs, target: usize) -> Result<MsgResult, i64> {
    let bytes = read_parcel(args)?;
    let parcel = decode_parcel(&bytes)?;
    let name = registry_name(&parcel)?;
    registry::unregister(task::current(), target, &name).map_err(registry_errno)?;
    Ok(MsgResult::default())
}

/// `OP_LIST`: encode the table as a parcel whose body has one `ENTRY` record
/// per name, then copy it into the caller's buffer. The wire shape is shared
/// with `user/src/messenger/`, which always offers a large enough buffer.
pub(super) fn registry_list(args: &MsgArgs) -> Result<MsgResult, i64> {
    let entries = registry::list();
    let mut body = Encoder::new();
    for entry in &entries {
        let mut record = Encoder::new();
        record
            .string(registry::field::NAME, &entry.name)
            .map_err(|_| errno::E2BIG)?;
        record
            .u64(registry::field::OBJECT, entry.object_id)
            .map_err(|_| errno::E2BIG)?;
        record
            .u64(registry::field::OWNER, entry.owner_slot as u64)
            .map_err(|_| errno::E2BIG)?;
        let mut interfaces = Encoder::new();
        for interface in &entry.interfaces {
            interfaces
                .u64(registry::field::INTERFACES, *interface)
                .map_err(|_| errno::E2BIG)?;
        }
        record
            .array(registry::field::INTERFACES, &interfaces)
            .map_err(|_| errno::E2BIG)?;
        record
            .u64(
                registry::field::LEASE_REMAINING,
                entry.lease_remaining.unwrap_or(0),
            )
            .map_err(|_| errno::E2BIG)?;
        body.record(registry::field::ENTRY, &record)
            .map_err(|_| errno::E2BIG)?;
    }
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
        body: body.finish(),
        handles: Vec::new(),
        buffers: Vec::new(),
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
