//! The declared-object gate (issue #516, `docs/messenger-core-plan.md` 2.3):
//! a request carries exactly the objects its `.midl` method declares, in
//! kind and order, and a refusal moves nothing.

use super::*;
use crate::ipc::channels::{self, declared, Error as ChannelError};
use crate::ipc::handles::{self, HandleKind};
use crate::ipc::shared;
use libmessenger::{flags, Header, Object, ObjectKind, Parcel, VERSION};
use messenger_generated::os_lazy_confd_v1 as confd;
use messenger_generated::os_lazy_display_v1 as display;
use messenger_generated::os_lazy_net_nic_v1 as nic;

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

/// A request parcel for `(interface, method)` carrying `objects`.
fn request(interface: u64, method: u32, objects: Vec<Object>) -> Result<Vec<u8>, String> {
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
        objects,
    };
    let mut bytes = Vec::new();
    parcel.encode(&mut bytes).map_err(|error| error.message())?;
    Ok(bytes)
}

/// A movable channel end (one side of a fresh pair; the other stays open).
fn end() -> Result<u64, String> {
    channels::create().map(|(end, _)| end).map_err(reason)
}

/// A one-page shared buffer.
fn buffer() -> Result<u64, String> {
    shared::create(4096).map_err(|error| String::from(error.message()))
}

/// Receive everything queued on `server`, closing what it installed, and
/// return how many messages arrived.
fn drain(server: u64) -> Result<usize, String> {
    let mut count = 0;
    while let Some(message) = channels::try_recv(server).map_err(reason)? {
        for handle in &message.objects {
            match handles::get(*handle).map(|entry| entry.kind) {
                Ok(HandleKind::Buffer) => shared::close(*handle).ok(),
                _ => channels::close_endpoint(*handle).ok(),
            };
        }
        count += 1;
    }
    Ok(count)
}

/// The kernel's table agrees with each interface's generated `*_OBJECTS`,
/// and an unknown interface declares nothing.
pub fn transfer_gate_table_matches_idl() -> Result<(), String> {
    let cases: [(u64, u32, &[ObjectKind]); 5] = [
        (
            display::INTERFACE_ID,
            display::METHOD_CREATESURFACE,
            display::CREATE_SURFACE_OBJECTS,
        ),
        (
            display::INTERFACE_ID,
            display::METHOD_ATTACHBUFFER,
            display::ATTACH_BUFFER_OBJECTS,
        ),
        (display::INTERFACE_ID, display::METHOD_COMMIT, &[]),
        (
            nic::INTERFACE_ID,
            nic::METHOD_ATTACHRING,
            nic::ATTACH_RING_OBJECTS,
        ),
        (confd::INTERFACE_ID, confd::METHOD_GET, &[]),
    ];
    for (interface, method, kinds) in cases {
        check!(
            declared::declared(interface, method) == kinds,
            "{interface:#x}/{method}: kernel {:?}, IDL {kinds:?}",
            declared::declared(interface, method)
        );
    }
    check!(
        nic::ATTACH_RING_OBJECTS == [ObjectKind::Buffer, ObjectKind::Channel],
        "AttachRing declares {:?}",
        nic::ATTACH_RING_OBJECTS
    );
    check!(
        declared::declared(UNKNOWN_INTERFACE, 1).is_empty(),
        "an unknown interface declares objects"
    );
    check!(
        !messenger_generated::DECLARED_OBJECTS.is_empty(),
        "the generated table is empty"
    );
    Ok(())
}

/// A request whose list is exactly the declaration passes and is delivered,
/// its objects installed in declared order.
pub fn transfer_gate_declared_request_passes() -> Result<(), String> {
    fresh()?;
    let (client, server) = channels::create().map_err(reason)?;
    let events = end()?;
    let bytes = request(
        display::INTERFACE_ID,
        display::METHOD_CREATESURFACE,
        vec![Object::Channel(events)],
    )?;
    channels::send(client, &bytes).map_err(reason)?;
    check!(
        handles::get(events).is_err(),
        "the declared channel end did not move"
    );
    let message = channels::try_recv(server)
        .map_err(reason)?
        .ok_or("the declared request never arrived")?;
    check!(
        message.objects.len() == 1,
        "delivered {} objects",
        message.objects.len()
    );
    channels::close_endpoint(message.objects[0]).ok();

    // AttachRing declares a buffer then a channel, in that order.
    let notify = end()?;
    let ring = buffer()?;
    let bytes = request(
        nic::INTERFACE_ID,
        nic::METHOD_ATTACHRING,
        vec![Object::Buffer(ring), Object::Channel(notify)],
    )?;
    channels::send(client, &bytes).map_err(reason)?;
    let message = channels::try_recv(server)
        .map_err(reason)?
        .ok_or("AttachRing never arrived")?;
    let kinds: Vec<HandleKind> = message
        .objects
        .iter()
        .map(|handle| handles::get(*handle).map(|entry| entry.kind))
        .collect::<Result<_, _>>()
        .map_err(|error| String::from(error.message()))?;
    check!(
        kinds == [HandleKind::Buffer, HandleKind::Channel],
        "AttachRing installed {kinds:?}"
    );
    shared::close(message.objects[0]).ok();
    channels::close_endpoint(message.objects[1]).ok();
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
        outcome == Err(ChannelError::UndeclaredObject),
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

/// The comparison is exact: an object on a method that declares none, the
/// wrong kind, an extra object, a missing object, and the right kinds in
/// the wrong order are all refused before anything moves.
pub fn transfer_gate_undeclared_and_excess_refused() -> Result<(), String> {
    fresh()?;
    let (client, server) = channels::create().map_err(reason)?;
    let first = end()?;
    let second = end()?;
    let ring = buffer()?;
    let refs = shared::info(ring)
        .map_err(|error| String::from(error.message()))?
        .refs;

    // A method that declares none.
    let undeclared = request(
        confd::INTERFACE_ID,
        confd::METHOD_GET,
        vec![Object::Channel(first)],
    )?;
    expect_refused(client, server, &undeclared, &[first])?;
    let commit = request(
        display::INTERFACE_ID,
        display::METHOD_COMMIT,
        vec![Object::Buffer(ring)],
    )?;
    expect_refused(client, server, &commit, &[ring])?;
    // The wrong kind: a buffer where a channel is declared, and the reverse.
    let wrong = request(
        display::INTERFACE_ID,
        display::METHOD_CREATESURFACE,
        vec![Object::Buffer(ring)],
    )?;
    expect_refused(client, server, &wrong, &[ring])?;
    let wrong = request(
        display::INTERFACE_ID,
        display::METHOD_ATTACHBUFFER,
        vec![Object::Channel(first)],
    )?;
    expect_refused(client, server, &wrong, &[first])?;
    // An extra object.
    let two = request(
        display::INTERFACE_ID,
        display::METHOD_CREATESURFACE,
        vec![Object::Channel(first), Object::Channel(second)],
    )?;
    expect_refused(client, server, &two, &[first, second])?;
    let extra_buffer = request(
        display::INTERFACE_ID,
        display::METHOD_CREATESURFACE,
        vec![Object::Channel(first), Object::Buffer(ring)],
    )?;
    expect_refused(client, server, &extra_buffer, &[first, ring])?;
    // A missing object: fewer than declared is not the receiver's call.
    let missing = request(
        display::INTERFACE_ID,
        display::METHOD_CREATESURFACE,
        Vec::new(),
    )?;
    expect_refused(client, server, &missing, &[])?;
    let half = request(
        nic::INTERFACE_ID,
        nic::METHOD_ATTACHRING,
        vec![Object::Buffer(ring)],
    )?;
    expect_refused(client, server, &half, &[ring])?;
    // The right kinds in the wrong order.
    let swapped = request(
        nic::INTERFACE_ID,
        nic::METHOD_ATTACHRING,
        vec![Object::Channel(first), Object::Buffer(ring)],
    )?;
    expect_refused(client, server, &swapped, &[first, ring])?;
    let info = shared::info(ring).map_err(|error| String::from(error.message()))?;
    check!(
        info.refs == refs,
        "a refused buffer kept a reference: {info:?}"
    );
    channels::close_endpoint(first).ok();
    channels::close_endpoint(second).ok();
    shared::close(ring).ok();
    channels::reset();
    shared::reset();
    Ok(())
}

/// An interface no `.midl` declares may not carry objects, but plain
/// requests to it still pass; a synchronous call is gated like a send.
pub fn transfer_gate_unknown_interface_and_calls() -> Result<(), String> {
    fresh()?;
    let (client, server) = channels::create().map_err(reason)?;
    let handle = end()?;
    let carrying = request(UNKNOWN_INTERFACE, 1, vec![Object::Channel(handle)])?;
    expect_refused(client, server, &carrying, &[handle])?;
    let plain = request(UNKNOWN_INTERFACE, 1, Vec::new())?;
    channels::send(client, &plain).map_err(reason)?;
    check!(drain(server)? == 1, "an object-free request was refused");

    let before = channels::stats();
    let call = channels::begin_call(client, 1, &carrying, None);
    check!(
        call == Err(ChannelError::UndeclaredObject),
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
    channels::close_endpoint(handle).ok();
    channels::reset();
    Ok(())
}

/// Flood undeclared objects of every shape: every one is refused, and the
/// sender's table, the buffer registry and the receiver's table end exactly
/// where they started.
pub fn transfer_gate_soak_leaks_nothing() -> Result<(), String> {
    const ROUNDS: u64 = 20_000;
    fresh()?;
    let me = task::current();
    let (client, server) = channels::create().map_err(reason)?;
    let ring = buffer()?;
    let handle = end()?;
    let held = handles::count_for_task(me);
    let buffers = shared::stats().buffers;
    let refused = declared::refused();
    for round in 0..ROUNDS {
        let bytes = match round % 4 {
            0 => request(
                confd::INTERFACE_ID,
                confd::METHOD_GET,
                vec![Object::Channel(handle)],
            )?,
            1 => request(
                UNKNOWN_INTERFACE,
                7,
                vec![Object::Channel(handle), Object::Buffer(ring)],
            )?,
            2 => request(
                display::INTERFACE_ID,
                display::METHOD_CREATESURFACE,
                vec![Object::Channel(handle), Object::Buffer(ring)],
            )?,
            _ => request(
                display::INTERFACE_ID,
                display::METHOD_COMMIT,
                vec![Object::Buffer(ring)],
            )?,
        };
        let outcome = channels::send(client, &bytes);
        check!(
            outcome == Err(ChannelError::UndeclaredObject),
            "round {round}: {outcome:?}"
        );
        check!(
            handles::get(handle).is_ok() && shared::info(ring).is_ok(),
            "round {round}: a refused request moved a handle"
        );
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
