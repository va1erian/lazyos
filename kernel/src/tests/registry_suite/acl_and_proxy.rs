//! The ACL hook gating registry ops, `list` reflecting state, the
//! `messengerd` proxy-registration path, and the native syscall
//! surface end to end.

use super::*;

/// The ACL hook gates a registry op before the table is touched: a policy
/// that does not cover the caller denies the register and audits it.
pub fn acl_denies_register() -> Result<(), String> {
    fresh()?;
    acl::load(&[acl::Rule {
        actor: 2000,
        interface_id: registry::INTERFACE,
        method: registry::method::REGISTER,
        allow: true,
    }]);
    credentials::set(
        task::KERNEL_TASK,
        credentials::Cred::new(1000, 100, 0, 0, 0),
    );
    in_space(|| -> Result<(), String> {
        let (_service, callable) = channels::create().map_err(friendly)?;
        let request = register_parcel("os.example.denied", callable, &[9], 0)?;
        write_bytes(REQUEST, &request);
        let before = audit::count();
        let args = MsgArgs {
            parcel_ptr: REQUEST,
            parcel_len: request.len() as u64,
            ..MsgArgs::default()
        };
        let (code, result) = dispatch(OP_REGISTER, &args);
        check!(
            code == failed(errno::EACCES),
            "denied register -> {code:#x}"
        );
        check!(
            result.status == -errno::EACCES,
            "the denial status is {}",
            result.status
        );
        check!(
            registry::list().is_empty(),
            "a denied register touched the table"
        );
        check!(
            audit::count() == before + 1,
            "the denial was not audited: {} -> {}",
            before,
            audit::count()
        );
        let event = *audit::recent(1).first().ok_or("no audit event")?;
        check!(
            !event.allow
                && event.interface_id == registry::INTERFACE
                && event.method == registry::method::REGISTER,
            "the audit event is {event:?}"
        );
        Ok(())
    })
}

/// `list` reflects registrations, interfaces, owners and leases; a
/// different owner cannot take a taken name, and an unrelated task without
/// the admin capability cannot unregister it.
pub fn list_reflects_state() -> Result<(), String> {
    fresh()?;
    let (_a, endpoint_a) = channels::create().map_err(friendly)?;
    let (_b, endpoint_b) = channels::create().map_err(friendly)?;
    let entry_a = handles::get(endpoint_a).map_err(friendly)?;
    let entry_b = handles::get(endpoint_b).map_err(friendly)?;
    registry::register(
        task::KERNEL_TASK,
        "os.example.alpha",
        entry_a.kind,
        entry_a.rights,
        entry_a.object_id,
        &[1, 2],
        100,
    )
    .map_err(reason)?;
    registry::register(
        task::KERNEL_TASK,
        "os.example.beta",
        entry_b.kind,
        entry_b.rights,
        entry_b.object_id,
        &[3],
        0,
    )
    .map_err(reason)?;

    let entries = registry::list();
    check!(entries.len() == 2, "list has {} entries", entries.len());
    check!(
        entries[0].name == "os.example.alpha" && entries[1].name == "os.example.beta",
        "list order is {:?}",
        entries.iter().map(|entry| &entry.name).collect::<Vec<_>>()
    );
    check!(
        entries[0].interfaces == vec![1, 2] && entries[1].interfaces == vec![3],
        "interfaces are {:?} / {:?}",
        entries[0].interfaces,
        entries[1].interfaces
    );
    check!(
        entries[0].owner_slot == task::KERNEL_TASK,
        "the owner is {}",
        entries[0].owner_slot
    );
    check!(
        entries[0].lease_remaining.is_some(),
        "alpha lost its lease in the listing"
    );
    check!(
        entries[1].lease_remaining.is_none(),
        "beta gained a lease in the listing"
    );
    let stats = registry::stats();
    check!(
        stats.entries == 2 && stats.leases == 1 && stats.registrations == 2,
        "registry stats are {stats:?}"
    );

    // A second owner cannot take the name; a stranger cannot withdraw it.
    let child = task::spawn_fork().map_err(to_string)?;
    credentials::set(child, credentials::Cred::new(1000, 100, 0, 0, 0));
    check!(
        registry::register(
            child,
            "os.example.alpha",
            entry_a.kind,
            entry_a.rights,
            entry_a.object_id,
            &[],
            0,
        ) == Err(RegistryError::NameTaken),
        "a second owner took a registered name"
    );
    check!(
        registry::unregister(child, task::KERNEL_TASK, "os.example.alpha")
            == Err(RegistryError::NotOwner),
        "a stranger unregistered a name"
    );

    // The owner withdraws one; state follows.
    registry::unregister(task::KERNEL_TASK, task::KERNEL_TASK, "os.example.alpha")
        .map_err(reason)?;
    let entries = registry::list();
    check!(
        entries.len() == 1 && entries[0].name == "os.example.beta",
        "list after unregister is {:?}",
        entries.iter().map(|entry| &entry.name).collect::<Vec<_>>()
    );
    check!(
        registry::stats().unregistrations == 1,
        "the unregistration was not counted"
    );
    registry::unregister(task::KERNEL_TASK, task::KERNEL_TASK, "os.example.beta")
        .map_err(reason)?;
    check!(registry::list().is_empty(), "the table is not empty");
    Ok(())
}

/// The `messengerd` proxy path: a task holding `CAP_IPC_CONTROL` names
/// another task's slot as the target, so the kernel reads the client's
/// endpoint handle from the client's table, records the client as owner,
/// and opens a resolved handle back into the client. Without the
/// capability the same request is refused.
pub fn proxy_registers_for_client() -> Result<(), String> {
    fresh()?;
    let client = task::spawn_fork().map_err(to_string)?;

    // The client owns a channel and keeps the receiving side.
    task::harness::switch_current(client);
    let (_service, callable) = channels::create().map_err(friendly)?;
    task::harness::switch_current(task::KERNEL_TASK);
    let published = handles::get_for_task(client, callable).map_err(friendly)?;

    in_space(|| -> Result<(), String> {
        // Register with the client's slot as target: the proxy is the
        // caller, but the name must belong to the client.
        let request = register_parcel("os.example.proxy", callable, &[5], 0)?;
        write_bytes(REQUEST, &request);
        let args = MsgArgs {
            txn_id: client as u64,
            parcel_ptr: REQUEST,
            parcel_len: request.len() as u64,
            ..MsgArgs::default()
        };
        let (code, _) = dispatch(OP_REGISTER, &args);
        check!(code == 0, "proxied register -> {code:#x}");
        let entries = registry::list();
        check!(
            entries.len() == 1 && entries[0].owner_slot == client,
            "the owner is not the client: {:?}",
            entries
        );

        // Resolve with the client's slot as target: the handle must land in
        // the *client's* table, not the proxy's.
        let request = string_parcel(registry::method::RESOLVE, "os.example.proxy")?;
        write_bytes(REQUEST, &request);
        let args = MsgArgs {
            txn_id: client as u64,
            parcel_ptr: REQUEST,
            parcel_len: request.len() as u64,
            ..MsgArgs::default()
        };
        let (code, result) = dispatch(OP_RESOLVE, &args);
        check!(code == 0, "proxied resolve -> {code:#x}");
        let resolved = handles::get_for_task(client, result.value).map_err(friendly)?;
        check!(
            resolved.object_id == published.object_id,
            "the proxied handle names a different object"
        );
        check!(
            handles::count_for_task(client) == 3,
            "the client holds {} handles, expected 3",
            handles::count_for_task(client)
        );

        // Without the capability the same target is refused.
        credentials::set(
            task::KERNEL_TASK,
            credentials::Cred::new(1000, 100, 0, 0, 0),
        );
        let request = string_parcel(registry::method::RESOLVE, "os.example.proxy")?;
        write_bytes(REQUEST, &request);
        let args = MsgArgs {
            txn_id: client as u64,
            parcel_ptr: REQUEST,
            parcel_len: request.len() as u64,
            ..MsgArgs::default()
        };
        let (code, _) = dispatch(OP_RESOLVE, &args);
        check!(
            code == failed(errno::EPERM),
            "an unprivileged proxy -> {code:#x}"
        );

        // Unregister through the proxy: the capability authorises, but the
        // name still belongs to the client, so the client's slot must stay
        // the owner named by the request.
        credentials::reset_for_task(task::KERNEL_TASK);
        let request = string_parcel(registry::method::UNREGISTER, "os.example.proxy")?;
        write_bytes(REQUEST, &request);
        let args = MsgArgs {
            txn_id: client as u64,
            parcel_ptr: REQUEST,
            parcel_len: request.len() as u64,
            ..MsgArgs::default()
        };
        let (code, _) = dispatch(OP_UNREGISTER, &args);
        check!(code == 0, "proxied unregister -> {code:#x}");
        check!(registry::list().is_empty(), "the table is not empty");
        Ok(())
    })?;

    task::harness::finish(client, 0);
    check!(task::reap_child().is_some(), "the client was not reapable");
    Ok(())
}

/// The ops over the native gate: register, resolve, list, unknown-name and
/// unregister all round-trip through the ABI blocks and the TLV bodies.
pub fn syscall_roundtrip() -> Result<(), String> {
    fresh()?;
    in_space(|| -> Result<(), String> {
        let (_service, callable) = channels::create().map_err(friendly)?;
        let published = handles::get(callable).map_err(friendly)?;

        // Register through OP_REGISTER; the reply names the object.
        let request = register_parcel("os.example.sys", callable, &[7, 8], 0)?;
        write_bytes(REQUEST, &request);
        let args = MsgArgs {
            parcel_ptr: REQUEST,
            parcel_len: request.len() as u64,
            ..MsgArgs::default()
        };
        let (code, result) = dispatch(OP_REGISTER, &args);
        check!(code == 0, "register -> {code:#x}");
        check!(
            result.value == published.object_id,
            "register returned object {} (expected {})",
            result.value,
            published.object_id
        );

        // Resolve through OP_RESOLVE duplicates the handle for this task.
        let request = string_parcel(registry::method::RESOLVE, "os.example.sys")?;
        write_bytes(REQUEST, &request);
        let args = MsgArgs {
            parcel_ptr: REQUEST,
            parcel_len: request.len() as u64,
            ..MsgArgs::default()
        };
        let (code, result) = dispatch(OP_RESOLVE, &args);
        check!(code == 0, "resolve -> {code:#x}");
        check!(
            result.value != callable,
            "resolve reused the registered handle"
        );
        let copy = handles::get(result.value).map_err(friendly)?;
        check!(
            copy.object_id == published.object_id,
            "the resolved handle names a different object"
        );

        // List through OP_LIST writes an encoded parcel of records.
        let args = MsgArgs {
            buf_ptr: LIST_BUF,
            buf_cap: 4096,
            ..MsgArgs::default()
        };
        let (code, result) = dispatch(OP_LIST, &args);
        check!(code == 0, "list -> {code:#x}");
        let bytes = read_bytes(LIST_BUF, result.bytes as usize);
        let parcel = Parcel::decode(&bytes).map_err(friendly)?;
        check!(
            parcel.header.interface_id == registry::INTERFACE
                && parcel.header.method == registry::method::LIST,
            "the list parcel header is {:?}",
            parcel.header
        );
        let mut decoder = Decoder::new(&parcel.body);
        let mut records = 0;
        while let Some(field) = decoder.next().map_err(friendly)? {
            if field.kind == Kind::Struct && field.id == registry::field::ENTRY {
                records += 1;
            }
        }
        check!(records == 1, "the list body has {records} records");

        // An unknown name is a friendly -ENOENT.
        let request = string_parcel(registry::method::RESOLVE, "no.such.service")?;
        write_bytes(REQUEST, &request);
        let args = MsgArgs {
            parcel_ptr: REQUEST,
            parcel_len: request.len() as u64,
            ..MsgArgs::default()
        };
        let (code, result) = dispatch(OP_RESOLVE, &args);
        check!(
            code == failed(errno::ENOENT),
            "unknown resolve -> {code:#x}"
        );
        check!(
            result.status == -errno::ENOENT,
            "the unknown-name status is {}",
            result.status
        );

        // Unregister through OP_UNREGISTER empties the table.
        let request = string_parcel(registry::method::UNREGISTER, "os.example.sys")?;
        write_bytes(REQUEST, &request);
        let args = MsgArgs {
            parcel_ptr: REQUEST,
            parcel_len: request.len() as u64,
            ..MsgArgs::default()
        };
        let (code, _) = dispatch(OP_UNREGISTER, &args);
        check!(code == 0, "unregister -> {code:#x}");
        check!(registry::list().is_empty(), "the table is not empty");
        Ok(())
    })
}
