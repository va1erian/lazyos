//! The declared-transfer gate (issue #516): a request carries only the
//! handles and buffers its `.midl` method declares, and a refusal moves
//! nothing.

use super::*;
use crate::ipc::channels::{self, declared, Error as ChannelError};
use crate::ipc::handles::{self, rights, HandleKind};
use crate::ipc::shared;
use libmessenger::{flags, BufferDesc, Header, Parcel, VERSION};
use messenger_generated::os_lazy_confd_v1 as confd;
use messenger_generated::os_lazy_display_v1 as display;
use messenger_generated::os_lazy_net_nic_v1 as nic;
use messenger_generated::transfers::Transfers;

/// An interface no `.midl` file declares.
const UNKNOWN_INTERFACE: u64 = 0x5eed_0000_dead_beef;

fn reason(error: ChannelError) -> String {
    error.message().into()
}

/// Start from empty registries, as the other channel suites do.
fn fresh() -> Result<(), String> {
    task::register_kernel();
    task::harness::reset();
    channels::reset();
    shared::reset();
    handles::reset_for_task(task::current());
    Ok(())
}

/// A request parcel for `(interface, method)` carrying the given transfers.
fn request(
    interface: u64,
    method: u32,
    handles: Vec<u64>,
    buffers: Vec<BufferDesc>,
) -> Result<Vec<u8>, String> {
    let parcel = Parcel {
        header: Header {
            version: VERSION,
            flags: flags::ONE_WAY,
            interface_id: interface,
            method,
            txn_id: 0,
            reply_to: 0,
            deadline_ns: 0,
        },
        body: Vec::new(),
        handles,
        buffers,
    };
    let mut bytes = Vec::new();
    parcel.encode(&mut bytes).map_err(|error| error.message())?;
    Ok(bytes)
}

/// A movable object handle.
fn object(id: u64) -> Result<u64, String> {
    handles::open(HandleKind::Object, rights::CALL | rights::TRANSFER, id)
        .map_err(|error| error.message().into())
}

/// A one-page shared buffer and a descriptor for all of it.
fn buffer() -> Result<(u64, BufferDesc), String> {
    let handle = shared::create(4096, shared::flags::READ | shared::flags::WRITE)
        .map_err(|error| String::from(error.message()))?;
    let desc = BufferDesc {
        handle,
        offset: 0,
        len: 4096,
        flags: 0,
    };
    Ok((handle, desc))
}

/// Receive everything queued on `server`, closing what it installed, and
/// return how many messages arrived.
fn drain(server: u64) -> Result<usize, String> {
    let mut count = 0;
    while let Some(message) = channels::try_recv(server).map_err(reason)? {
        for handle in &message.handles {
            handles::close(*handle).ok();
            shared::close(*handle).ok();
        }
        for desc in &message.buffers {
            shared::close(desc.handle).ok();
        }
        count += 1;
    }
    Ok(count)
}

/// The kernel's table agrees with each interface's generated
/// `request_transfers`, and an unknown interface declares nothing.
pub fn transfer_gate_table_matches_idl() -> Result<(), String> {
    let pairs = [
        (display::INTERFACE_ID, display::METHOD_CREATESURFACE),
        (display::INTERFACE_ID, display::METHOD_ATTACHBUFFER),
        (display::INTERFACE_ID, display::METHOD_COMMIT),
        (nic::INTERFACE_ID, nic::METHOD_ATTACHRING),
        (confd::INTERFACE_ID, confd::METHOD_GET),
    ];
    for (interface, method) in pairs {
        let module = if interface == display::INTERFACE_ID {
            display::request_transfers(method)
        } else if interface == nic::INTERFACE_ID {
            nic::request_transfers(method)
        } else {
            confd::request_transfers(method)
        };
        check!(
            declared::declared(interface, method) == module,
            "{interface:#x}/{method}: kernel {:?}, IDL {module:?}",
            declared::declared(interface, method)
        );
    }
    check!(
        declared::declared(UNKNOWN_INTERFACE, 1) == Transfers::NONE,
        "an unknown interface declares transfers"
    );
    check!(
        !messenger_generated::DECLARED_TRANSFERS.is_empty(),
        "the generated table is empty"
    );
    Ok(())
}

/// Declared transfers pass and are delivered; fewer than declared pass too.
pub fn transfer_gate_declared_request_passes() -> Result<(), String> {
    fresh()?;
    let (client, server) = channels::create().map_err(reason)?;
    let events = object(0x516)?;
    let bytes = request(
        display::INTERFACE_ID,
        display::METHOD_CREATESURFACE,
        vec![events],
        Vec::new(),
    )?;
    channels::send(client, &bytes).map_err(reason)?;
    check!(
        handles::get(events).is_err(),
        "the declared handle did not move"
    );
    let message = channels::try_recv(server)
        .map_err(reason)?
        .ok_or("the declared request never arrived")?;
    check!(
        message.handles.len() == 1,
        "delivered {} handles",
        message.handles.len()
    );
    handles::close(message.handles[0]).ok();

    // AttachRing declares one channel and one buffer.
    let notify = object(0x517)?;
    let (ring, desc) = buffer()?;
    let bytes = request(
        nic::INTERFACE_ID,
        nic::METHOD_ATTACHRING,
        vec![notify],
        vec![desc],
    )?;
    channels::send(client, &bytes).map_err(reason)?;
    check!(drain(server)? == 1, "AttachRing was not delivered");

    // An optional transfer left out is the receiver's call, not the gate's.
    let bytes = request(
        display::INTERFACE_ID,
        display::METHOD_CREATESURFACE,
        Vec::new(),
        Vec::new(),
    )?;
    channels::send(client, &bytes).map_err(reason)?;
    check!(
        drain(server)? == 1,
        "a request with fewer transfers was refused"
    );
    shared::close(ring).ok();
    channels::reset();
    shared::reset();
    Ok(())
}

/// Send `bytes` and require the gate's refusal with nothing delivered and
/// the sender's `kept` handles intact.
fn expect_refused(client: u64, server: u64, bytes: &[u8], kept: &[u64]) -> Result<(), String> {
    let before = declared::refused();
    let outcome = channels::send(client, bytes);
    check!(
        outcome == Err(ChannelError::UndeclaredTransfer),
        "the gate let it through: {outcome:?}"
    );
    check!(
        declared::refused() == before + 1,
        "the refusal was not counted"
    );
    for handle in kept {
        check!(
            handles::get(*handle).is_ok(),
            "the refused request moved handle {handle}"
        );
    }
    check!(drain(server)? == 0, "the refused request was delivered");
    Ok(())
}

/// A handle on a method that declares none, a second handle, and a buffer a
/// method does not declare are all refused before anything moves.
pub fn transfer_gate_undeclared_and_excess_refused() -> Result<(), String> {
    fresh()?;
    let (client, server) = channels::create().map_err(reason)?;
    let first = object(1)?;
    let second = object(2)?;
    let (ring, desc) = buffer()?;
    let refs = shared::info(ring)
        .map_err(|error| String::from(error.message()))?
        .refs;

    let undeclared = request(
        confd::INTERFACE_ID,
        confd::METHOD_GET,
        vec![first],
        Vec::new(),
    )?;
    expect_refused(client, server, &undeclared, &[first])?;
    let commit = request(
        display::INTERFACE_ID,
        display::METHOD_COMMIT,
        Vec::new(),
        vec![desc],
    )?;
    expect_refused(client, server, &commit, &[ring])?;
    let two = request(
        display::INTERFACE_ID,
        display::METHOD_CREATESURFACE,
        vec![first, second],
        Vec::new(),
    )?;
    expect_refused(client, server, &two, &[first, second])?;
    let extra_buffer = request(
        display::INTERFACE_ID,
        display::METHOD_CREATESURFACE,
        vec![first],
        vec![desc],
    )?;
    expect_refused(client, server, &extra_buffer, &[first, ring])?;
    let info = shared::info(ring).map_err(|error| String::from(error.message()))?;
    check!(
        info.refs == refs,
        "a refused buffer kept a reference: {info:?}"
    );
    handles::close(first).ok();
    handles::close(second).ok();
    shared::close(ring).ok();
    channels::reset();
    shared::reset();
    Ok(())
}

/// An interface no `.midl` declares may not carry transfers, but plain
/// requests to it still pass; a synchronous call is gated like a send.
pub fn transfer_gate_unknown_interface_and_calls() -> Result<(), String> {
    fresh()?;
    let (client, server) = channels::create().map_err(reason)?;
    let handle = object(3)?;
    let carrying = request(UNKNOWN_INTERFACE, 1, vec![handle], Vec::new())?;
    expect_refused(client, server, &carrying, &[handle])?;
    let plain = request(UNKNOWN_INTERFACE, 1, Vec::new(), Vec::new())?;
    channels::send(client, &plain).map_err(reason)?;
    check!(drain(server)? == 1, "a transfer-free request was refused");

    let before = channels::stats();
    let call = channels::begin_call(client, 1, &carrying, None);
    check!(
        call == Err(ChannelError::UndeclaredTransfer),
        "an undeclared call was accepted: {call:?}"
    );
    let after = channels::stats();
    check!(
        after.calls == before.calls,
        "the refused call opened a transaction"
    );
    check!(
        handles::get(handle).is_ok(),
        "the refused call moved the handle"
    );
    check!(drain(server)? == 0, "the refused call was delivered");
    handles::close(handle).ok();
    channels::reset();
    Ok(())
}

/// Flood undeclared transfers of every shape: every one is refused, and the
/// sender's table, the buffer registry and the receiver's table end exactly
/// where they started.
pub fn transfer_gate_soak_leaks_nothing() -> Result<(), String> {
    const ROUNDS: u64 = 20_000;
    fresh()?;
    let me = task::current();
    let (client, server) = channels::create().map_err(reason)?;
    let held = handles::count_for_task(me);
    let buffers = shared::stats().buffers;
    let refused = declared::refused();
    for round in 0..ROUNDS {
        let handle = object(round)?;
        let (ring, desc) = buffer()?;
        let bytes = match round % 4 {
            0 => request(
                confd::INTERFACE_ID,
                confd::METHOD_GET,
                vec![handle],
                Vec::new(),
            )?,
            1 => request(UNKNOWN_INTERFACE, 7, vec![handle], vec![desc])?,
            2 => request(
                display::INTERFACE_ID,
                display::METHOD_CREATESURFACE,
                vec![handle, ring],
                Vec::new(),
            )?,
            _ => request(
                display::INTERFACE_ID,
                display::METHOD_COMMIT,
                Vec::new(),
                vec![desc],
            )?,
        };
        let outcome = channels::send(client, &bytes);
        check!(
            outcome == Err(ChannelError::UndeclaredTransfer),
            "round {round}: {outcome:?}"
        );
        check!(
            handles::get(handle).is_ok() && shared::info(ring).is_ok(),
            "round {round}: a refused transfer moved a handle"
        );
        handles::close(handle).map_err(|error| String::from(error.message()))?;
        shared::close(ring).map_err(|error| String::from(error.message()))?;
    }
    check!(
        declared::refused() == refused + ROUNDS,
        "refused {} of {ROUNDS}",
        declared::refused() - refused
    );
    check!(
        drain(server)? == 0,
        "a refused request reached the receiver"
    );
    check!(
        handles::count_for_task(me) == held,
        "the table holds {} handles, expected {held}",
        handles::count_for_task(me)
    );
    check!(
        shared::stats().buffers == buffers,
        "{} buffers live, expected {buffers}",
        shared::stats().buffers
    );
    let channel = channels::channel_stats(client).map_err(reason)?;
    check!(
        channel.drops == 0,
        "refusals reached the channel: {channel:?}"
    );
    serial_println!("TEST:transfer_gate_soak_leaks_nothing:INFO:rounds={ROUNDS} refused={ROUNDS}");
    channels::reset();
    shared::reset();
    Ok(())
}

pub(super) const CASES: &[(&str, Test)] = &[
    (
        "transfer_gate_table_matches_idl",
        transfer_gate_table_matches_idl,
    ),
    (
        "transfer_gate_declared_request_passes",
        transfer_gate_declared_request_passes,
    ),
    (
        "transfer_gate_undeclared_and_excess_refused",
        transfer_gate_undeclared_and_excess_refused,
    ),
    (
        "transfer_gate_unknown_interface_and_calls",
        transfer_gate_unknown_interface_and_calls,
    ),
    (
        "transfer_gate_soak_leaks_nothing",
        transfer_gate_soak_leaks_nothing,
    ),
];
