//! Per-connection Messenger channels (issue #483): `Connect(name)` gives each
//! client its own channel, so one client closing is not peer death for the
//! others.

use super::*;
use crate::ipc::channels::{self, Error as ChannelError};
use crate::ipc::connect;
use crate::ipc::handles::{self, HandleKind};
use crate::ipc::registry::{self, Error as RegistryError};
use crate::quota::{self, Resource};
use libmessenger::{Header, Parcel, ParcelView, VERSION};

const NAME: &str = "os.lazy.test.connect";

fn reason(error: ChannelError) -> String {
    error.message().into()
}

fn fresh() {
    task::register_kernel();
    task::harness::reset();
    channels::reset();
    registry::reset();
    quota::reset();
    handles::reset_for_task(task::current());
}

/// A service: a channel whose side 1 is registered under [`NAME`] for
/// clients, with the service listening on side 0. Returns the listen handle.
fn service() -> Result<u64, String> {
    let (listen, clients) = channels::create().map_err(reason)?;
    let entry = handles::get(clients).map_err(|error| String::from(error.message()))?;
    registry::register(
        task::current(),
        NAME,
        HandleKind::Channel,
        entry.rights,
        entry.object_id,
        &[],
        0,
    )
    .map_err(|error| String::from(error.message()))?;
    // The registry keeps the endpoint by object id; the service's own copy
    // of the client side is not needed.
    handles::close(clients).map_err(|error| String::from(error.message()))?;
    Ok(listen)
}

/// Accept the next connection on `listen`: the `Connected` notice and the
/// service end it carries.
fn accept(listen: u64) -> Result<u64, String> {
    let message = channels::try_recv(listen)
        .map_err(reason)?
        .ok_or("no Connected notice was queued")?;
    let view = ParcelView::parse(&message.bytes).map_err(|_| "unparsable notice")?;
    check!(
        view.header.interface_id == registry::INTERFACE
            && view.header.method == registry::method::CONNECTED,
        "the notice is {:#x}/{}",
        view.header.interface_id,
        view.header.method
    );
    check!(
        message.objects.len() == 1,
        "the notice carried {} objects",
        message.objects.len()
    );
    // The receiver decodes against the installed list: the field's index
    // names the end the kernel just gave it.
    let installed = [libmessenger::Object::Channel(message.objects[0])];
    let args = registry::wire::decode_connected_args(view.body(), &installed)
        .map_err(|_| "bad notice body")?;
    check!(args.name == NAME, "the notice names {:?}", args.name);
    check!(
        args.connection == message.objects[0],
        "the decoded connection is {} not {}",
        args.connection,
        message.objects[0]
    );
    Ok(args.connection)
}

fn request(text: u64) -> Result<Vec<u8>, String> {
    let mut body = libmessenger::Encoder::new();
    body.u64(1, text).map_err(|error| error.message())?;
    let parcel = Parcel {
        header: Header {
            version: VERSION,
            flags: 0,
            interface_id: 0x7e57_c044,
            method: 1,
            txn_id: 0,
            reply_to: 0,
            deadline_ns: 0,
        },
        body: body.finish(),
        objects: Vec::new(),
    };
    let mut bytes = Vec::new();
    parcel.encode(&mut bytes).map_err(|error| error.message())?;
    Ok(bytes)
}

/// One call from `client` served on `server`: begin, receive, reply, await.
fn round_trip(client: u64, server: u64, value: u64) -> Result<(), String> {
    round_trip_from(task::current(), client, server, value)
}

/// [`round_trip`] with the client in task `slot` and the service in the
/// current task.
fn round_trip_from(slot: usize, client: u64, server: u64, value: u64) -> Result<(), String> {
    let service = task::current();
    let bytes = request(value)?;
    task::harness::switch_current(slot);
    let txn = channels::begin_call(client, 1, &bytes, None);
    task::harness::switch_current(service);
    let txn = txn.map_err(reason)?;
    let message = channels::try_recv(server)
        .map_err(reason)?
        .ok_or("the call never reached the service")?;
    check!(
        message.txn == Some(txn),
        "the service saw {:?}",
        message.txn
    );
    check!(
        message.sender == slot,
        "the call came from slot {}",
        message.sender
    );
    channels::reply(txn, &bytes).map_err(reason)?;
    task::harness::switch_current(slot);
    let reply = channels::await_reply(txn);
    task::harness::switch_current(service);
    check!(reply.map_err(reason)? == bytes, "the reply was mangled");
    Ok(())
}

/// Two clients (two tasks) connect to the same name; one closes its
/// connection and the other's calls still succeed. The service sees peer
/// death on the closed connection only.
pub fn connect_one_client_closing_spares_the_other() -> Result<(), String> {
    fresh();
    let listen = service()?;
    let me = task::current();
    let other = task::spawn_fork().map_err(|error| format!("spawn: {error}"))?;
    handles::reset_for_task(other);
    let outcome = two_clients(listen, me, other);
    handles::reset_for_task(other);
    task::harness::switch_current(me);
    task::harness::finish(other, 0);
    while task::reap_child().is_some() {}
    task::harness::reset();
    channels::reset();
    registry::reset();
    outcome
}

fn two_clients(listen: u64, me: usize, other: usize) -> Result<(), String> {
    let first = connect::connect(me, NAME).map_err(|error| String::from(error.message()))?;
    let second = connect::connect(other, NAME).map_err(|error| String::from(error.message()))?;
    let first_server = accept(listen)?;
    let second_server = accept(listen)?;
    round_trip(first, first_server, 1)?;
    round_trip_from(other, second, second_server, 2)?;

    channels::close_endpoint(first).map_err(reason)?;
    check!(
        channels::try_recv(first_server) == Err(ChannelError::PeerDied),
        "the service did not see the first client leave"
    );
    for value in 0..10 {
        round_trip_from(other, second, second_server, value)?;
    }
    // The name still accepts new connections.
    let third = connect::connect(me, NAME).map_err(|error| String::from(error.message()))?;
    let third_server = accept(listen)?;
    round_trip(third, third_server, 3)?;
    Ok(())
}

/// Unknown names, names whose service stopped listening, and a service that
/// never takes its notices: every connection fails cleanly, and an untaken
/// service end is closed so its client is not left waiting.
pub fn connect_refusals_and_orphans() -> Result<(), String> {
    fresh();
    let me = task::current();
    check!(
        connect::connect(me, "os.lazy.nobody") == Err(RegistryError::UnknownName),
        "an unknown name was connected"
    );
    let listen = service()?;
    let base = channels::counts().channels;
    let mut clients = Vec::new();
    for _ in 0..8 {
        clients.push(connect::connect(me, NAME).map_err(|error| String::from(error.message()))?);
    }
    check!(
        channels::counts().channels == base + 8,
        "eight connections made {} channels",
        channels::counts().channels - base
    );
    // The service goes away without accepting: its inbox (the notices and
    // the service ends in them) is dropped.
    channels::close_endpoint(listen).map_err(reason)?;
    for &client in &clients {
        let bytes = request(9)?;
        check!(
            channels::begin_call(client, 1, &bytes, None) == Err(ChannelError::PeerDied),
            "a client of a departed service could still call"
        );
        channels::close_endpoint(client).map_err(reason)?;
    }
    // Only the service's own (half-closed) channel remains.
    check!(
        channels::counts().channels == base,
        "{} channels left after every party closed (expected {base})",
        channels::counts().channels
    );
    check!(
        connect::connect(me, NAME) == Err(RegistryError::UnknownName),
        "a name whose service stopped listening was connected"
    );
    check!(
        quota::usage(0, Resource::QueueDepth) == 0,
        "dropped notices kept their queue charge"
    );
    registry::reset();
    channels::reset();
    Ok(())
}

/// Connect, call and close thousands of times, closing from either side:
/// channels, handles and queue charges return to where they started.
pub fn connect_soak_connect_close() -> Result<(), String> {
    const ROUNDS: u64 = 5_000;
    fresh();
    let me = task::current();
    let listen = service()?;
    let channels_before = channels::counts().channels;
    let handles_before = handles::count_for_task(me);
    for round in 0..ROUNDS {
        let client = connect::connect(me, NAME)
            .map_err(|error| format!("round {round}: {}", error.message()))?;
        let server = accept(listen).map_err(|error| format!("round {round}: {error}"))?;
        if round % 5 == 0 {
            round_trip(client, server, round)?;
        }
        // Alternate who hangs up first.
        let (first, second) = if round % 2 == 0 {
            (client, server)
        } else {
            (server, client)
        };
        channels::close_endpoint(first).map_err(reason)?;
        channels::close_endpoint(second).map_err(reason)?;
    }
    check!(
        channels::counts().channels == channels_before,
        "{} channels live after the soak, expected {channels_before}",
        channels::counts().channels
    );
    check!(
        handles::count_for_task(me) == handles_before,
        "{} handles held, expected {handles_before}",
        handles::count_for_task(me)
    );
    check!(
        quota::usage(0, Resource::QueueDepth) == 0 && quota::usage(0, Resource::QueueBytes) == 0,
        "the soak left queue charges behind"
    );
    serial_println!("TEST:connect_soak_connect_close:INFO:rounds={ROUNDS}");
    channels::reset();
    registry::reset();
    Ok(())
}

pub(super) const CASES: &[(&str, Test)] = &[
    (
        "connect_one_client_closing_spares_the_other",
        connect_one_client_closing_spares_the_other,
    ),
    ("connect_refusals_and_orphans", connect_refusals_and_orphans),
    ("connect_soak_connect_close", connect_soak_connect_close),
];
