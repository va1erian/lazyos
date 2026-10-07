//! Blocking userspace Messenger API over the native `messenger` syscall
//! (issue #69).
//!
//! This is the synchronous `Connection::call` / `Server::serve` shape from
//! `docs/messenger.md` section 15, layered straight on [`crate::sys`]: each
//! operation is one `int 0x80` with a small request/response block, and parcels
//! are encoded with [`libmessenger`]. The split `begin_call` + `await_reply`
//! ops are exposed too, because the kernel transaction already supports them
//! (`channels::begin_call`); the async API builds on the same pair later.
//!
//! The ABI blocks below mirror `kernel/src/ipc/syscalls.rs` field for field;
//! keep them in lockstep (the kernel pins the sizes at compile time).
//!
//! Deadlines are absolute PIT ticks (100 Hz), kernel-relative: `None` (encoded
//! as 0) waits forever, and userspace has no clock syscall yet, so callers that
//! want a timeout will need one before it is useful.
//!
//! # Module layout
//!
//! The core transport (request/reply ABI, [`Endpoint`], [`Server`]) lives in
//! [`types`] and [`endpoint`]; both are re-exported here so the split is
//! invisible to callers. Everything else is one `pub mod` per service
//! protocol, unchanged from before the split: [`registry`] (issue #89),
//! [`topics_client`] (issue #92), [`router`] (issue #93's interim broker),
//! [`services`] (`init`/`healthd`/`logd`), [`keyd`] (issue #102),
//! [`accounts`], [`logind`] (issue #101), [`display`] (issue #113),
//! [`mime`] and [`clipboard`] (issue #115).

mod endpoint;
mod message;
mod types;
pub mod wait;

pub use endpoint::{
    bootstrap, create_pair, fabric_stats, fabric_stats_with, global_stats, global_totals, Endpoint,
    Server,
};
pub use message::Message;
pub use types::{
    Error, FabricStats, MsgArgs, MsgResult, Result, Stats, TaskUsage, DEFAULT_BUFFER,
    EXPIRED_DEADLINE,
};

/// Re-export the wire message type: every service helper above returns or
/// accepts parcels, so callers need the type by name.
pub use libmessenger::Parcel;

/// Native syscall ops, matching the kernel's `ipc::syscalls::OP_*`.
pub mod op {
    /// Call a method and block until the reply arrives.
    pub const CALL: u64 = 1;
    /// Answer a pending transaction with a reply parcel.
    pub const REPLY: u64 = 2;
    /// Send a one-way message; never blocks.
    pub const SEND: u64 = 3;
    /// Receive the next message, blocking until one is queued.
    pub const RECV: u64 = 4;
    /// Cancel a pending transaction.
    pub const CANCEL: u64 = 5;
    /// Close an endpoint handle.
    pub const CLOSE_ENDPOINT: u64 = 6;
    /// `flags` of [`CLOSE_ENDPOINT`]: release the handle, and close the side
    /// only if no other handle names it.
    pub const CLOSE_RELEASE: u64 = 1;
    /// Create a fresh channel pair; both handles open in this task.
    pub const CREATE_PAIR: u64 = 7;
    /// Read channel counters (`handle = 0` means every live channel).
    pub const STATS: u64 = 8;
    /// Claim the boot-time client endpoint (first userspace task only).
    pub const BOOTSTRAP: u64 = 9;
    /// Register a call and park, returning the transaction id.
    pub const CALL_BEGIN: u64 = 10;
    /// Wait for a `CALL_BEGIN` transaction and return its reply.
    pub const CALL_AWAIT: u64 = 11;
    /// Global message totals in the compact 64-byte [`Stats`] shape.
    pub const TOTALS: u64 = 12;
    /// Publish a service name in the kernel registry (issue #89).
    pub const REGISTER: u64 = 13;
    /// Resolve a service name to a new handle.
    pub const RESOLVE: u64 = 14;
    /// Withdraw a service name.
    pub const UNREGISTER: u64 = 15;
    /// Snapshot the name table into the caller's buffer.
    pub const LIST: u64 = 16;
    /// Ask the kernel policy engine about every segment of a topic or filter
    /// (issue #92); the daemon uses this on behalf of a requesting client.
    pub const AUTHORIZE_TOPIC: u64 = 17;
    /// Replace every rule of one label (`CAP_IPC_CONTROL`; see [`super::policy`]).
    pub const ACL_LOAD: u64 = 18;
    /// Park until one of several endpoints (or a doorbell) is ready; see
    /// [`super::wait`].
    pub const WAIT: u64 = 19;
    /// Open a private connection to a registered name (issue #483).
    pub const CONNECT: u64 = 20;
}

/// `MsgArgs::txn_id` marker for registry ops: act on the calling task. A
/// different slot is the `messengerd` proxy path (kernel-side
/// `CAP_IPC_CONTROL`).
pub const REGISTRY_TARGET_SELF: u64 = u64::MAX;

/// Negative errno values the kernel returns; see the kernel's
/// `ipc::syscalls::errno`.
pub mod errno {
    pub const EPERM: i64 = 1;
    pub const ENOENT: i64 = 2;
    pub const EIO: i64 = 5;
    pub const E2BIG: i64 = 7;
    pub const EAGAIN: i64 = 11;
    pub const ENOMEM: i64 = 12;
    pub const EACCES: i64 = 13;
    pub const EFAULT: i64 = 14;
    pub const EBUSY: i64 = 16;
    pub const EEXIST: i64 = 17;
    pub const EINVAL: i64 = 22;
    pub const EPIPE: i64 = 32;
    pub const EDEADLK: i64 = 35;
    pub const ENOSYS: i64 = 38;
    pub const EBADMSG: i64 = 74;
    pub const ENOTSUP: i64 = 95;
    pub const ETIMEDOUT: i64 = 110;
    pub const ECANCELED: i64 = 125;
}

// ---------------------------------------------------------------------------
// Service name registry (issue #89)
// ---------------------------------------------------------------------------

/// Service name registry: the clients' and the daemon's view of the kernel
/// table (`docs/messenger.md` section 8).
///
/// Two paths reach the same table:
///
/// * the **direct** functions ([`register`], [`resolve`], [`unregister`],
///   [`list`]) are the native `register`/`resolve`/`unregister`/`list` ops; the
///   calling task is the owner, and `resolve` opens the discovered endpoint in
///   the caller's own handle table;
/// * [`Client`] talks to `messengerd` over the bootstrap channel. The daemon
///   is a thin, privileged proxy: it receives the request, forwards it to the
///   kernel with the *requester's* slot as the target (the kernel opens the
///   resolved handle straight into the requester's table), and answers with the
///   result or a friendly error.
///
/// [`serve_request`] is the daemon's half of that protocol: `messengerd` hands
/// it each received parcel and the kernel-stamped sender slot, and sends the
/// returned parcel back as the reply.
pub mod registry;

/// The kernel policy loader for label-keyed rules (`acl_load`).
pub mod policy;

// ---------------------------------------------------------------------------
// Pub/sub topics (issue #92)
// ---------------------------------------------------------------------------

/// Publish/subscribe topics (`docs/messenger.md` section 7.2).
///
/// ## Where the broker lives
///
/// Topics live in **userspace**, in `messengerd`, per the epic decision
/// recorded in section 20: the kernel's job is policy and message transport,
/// not naming, filters, QoS or retained state. The broker is addressed through
/// the well-known name [`NAME`] on the same bootstrap endpoint as the service
/// registry Ã¢â‚¬â€ the daemon dispatches on the parcel's interface id.
///
/// ## Delivery is pull-based, with kernel-mediated blocking
///
/// A subscription is a broker-side id, not a channel or a handle. The
/// subscriber asks for the next event with [`Subscription::next_event`], a
/// synchronous call to the broker:
///
/// * when an event is queued the broker replies immediately;
/// * when the queue is empty the broker **parks the transaction** and answers
///   it later, when a matching `Publish` arrives. The subscriber sleeps in the
///   kernel's wait queue with a real deadline, so `next_event(Some(ticks))`
///   times out cleanly and a slow subscriber never stalls the publisher.
///
/// This is why the broker's calls carry [`libmessenger::flags::ALLOW_NESTED`]:
/// the bootstrap channel is shared by every client, and one subscribed task
/// may be parked in `next_event` while another publishes on the same channel.
/// The kernel's per-channel cycle check would otherwise refuse the second
/// call as a deadlock.
///
/// ## QoS
///
/// [`Qos::Latest`] keeps one event (new replaces old), [`Qos::Buffered`] keeps
/// `N` and drops the oldest on overflow, [`Qos::Conflate`] coalesces the latest
/// event per publisher in its window, and [`Qos::Reliable`] keeps events until
/// the subscriber [`Subscription::ack`]s them, redelivering the head on the
/// next request. The broker counts every dropped event per subscriber;
/// [`Subscription::stats`] exposes the counter. There are no timers in
/// userspace yet, so `reliable` retirement is pull-driven (an event stays
/// outstanding until acked or the subscriber dies) Ã¢â‚¬â€ best-effort after peer
/// death is the documented limit.
///
/// ## Policy
///
/// Every publish and subscribe is checked segment by segment through the
/// kernel (`op::AUTHORIZE_TOPIC`), so `ipc::authorize` and its audit ring stay
/// the single policy choke point; the broker only maps `-EACCES` to its
/// friendly denial reply.
pub mod topics_client;

// ---------------------------------------------------------------------------
// Topics: the userspace event router (issue #93)
// ---------------------------------------------------------------------------

/// The userspace topic router the S2 services share (issue #93).
///
/// `docs/messenger.md` section 7 puts topics in `messengerd`, moved by the
/// kernel; that broker is still being built in `kernel/src/ipc` and the native
/// receive op does not yet surface transferred handles. Until it lands, the
/// supervisor services run this **interim router** over what the fabric does
/// support today:
///
/// * a service embeds a [`TopicBroker`] on its endpoint and registers the
///   endpoint's interface id in the kernel name registry;
/// * a subscriber calls [`Bus::subscribe`]; the broker hands it a unique sink
///   name (a counter with its prefix), the subscriber registers one end of its
///   own channel pair under that name with [`crate::messenger::registry`], and
///   the broker resolves the name (the kernel opens the endpoint straight into
///   the broker's table) and pushes [`Event`] parcels to it;
/// * topics are hierarchical with the spec's wildcards: `+` matches one
///   segment, `#` zero or more trailing segments;
/// * a broker retains the latest message per topic ([`TopicBroker::publish`]'s
///   `retained` flag) and replays matching retained values to a new subscriber,
///   which is what lets `logd` see service events that predate its start.
///
/// Service events use `system/events/<...>` (`system/events/service/<name>`
/// carries a service's state) and health state uses `system/health/<service>`,
/// as the platform plan names them. Only the transport changes when the
/// kernel/`messengerd` topic path lands; the topic names and payloads stay.
pub mod router;

// ---------------------------------------------------------------------------
// System service interfaces: init, healthd, logd (issue #93)
// ---------------------------------------------------------------------------

/// Wire shapes of the S2 system services (`init`, `healthd`, `logd`), shared by
/// the services themselves and by `messengerctl`.
///
/// The topic names are the platform plan's:
///
/// * `system/events/service/<name>` — a service's state changed (the typed
///   `ServiceEvent` from `idl/init.midl`);
/// * `system/events/security/denial` — the audit counters advanced (the
///   interim signal until the kernel exposes audit records to userspace);
/// * `system/health/<name>` — retained health row published by `healthd` (the
///   typed `HealthRecord` from `idl/healthd.midl`);
/// * `system/health/summary` — retained aggregate (worst status wins), the
///   same `HealthRecord` payload.
pub mod services;

// ---------------------------------------------------------------------------
// keyd: the secrets and crypto service (issue #102)
// ---------------------------------------------------------------------------

/// Client and wire shapes for `keyd`, the secrets and crypto service
/// (`docs/security-model.md` section 8).
///
/// The service owns password verifiers and key material in its own memory; the
/// protocol below only ever carries *operations* and their public results.
/// There is deliberately no request that returns key material and no reply
/// that carries a verifier: a `Wrap` returns a blob the client may store but
/// cannot open, an `Unwrap` happens inside `keyd`, and `Sign` returns a tag.
/// The kernel's `SHARE_ONLY` buffers back this contract once the userspace
/// buffer syscall lands (the kernel test proves the mapping rule today) and
/// `keyd` documents the interim copy-free path.
///
/// The same module is the daemon's protocol layer, so requests, replies and
/// error shapes round-trip through one implementation.
pub mod keyd;

/// Client and server shapes for the account database (issue #101 companion):
/// the `name`/`uid`/`gid`/`home`/`shell` table `accounts` owns.
///
/// The account database answers three questions: who is a name or uid
/// (`Lookup`), is this secret theirs (`Authenticate`), and can an admin create
/// a new account (`CreateUser`). A record is the `/etc/passwd` shape minus the
/// verifier: name, uid, gid, home, shell. The verifier stays in the daemon's
/// private table (or keyd).
pub mod accounts;

/// `logind` client and server shapes (issue #101): the session table and the
/// query `messengerctl sessions` renders.
pub mod logind;

// ---------------------------------------------------------------------------
// Display protocol (issue #113)
// ---------------------------------------------------------------------------

/// The display protocol (`docs/platform-plan.md` S4.4, issue #113): the
/// userspace compositor `xuid` owns the framebuffer through the kernel's device
/// grant and implements one Messenger interface, `os.lazy.display.v1`, defined
/// in `idl/display.midl` (issue #287); [`display::wire`] holds the generated
/// method ids and codecs.
///
/// ## Client and compositor
///
/// An app connects with [`display::Client::connect`], creates a surface (the
/// parcel transfers its **event endpoint**, so the compositor can send input
/// back), creates a shared pixel buffer with the `display` syscall, attaches it
/// and draws into it. `Commit` after each change is the "pixels are ready"
/// signal.
///
/// ## Input delivery
///
/// Input arrives as one-way messages on the event endpoint the app transferred:
/// [`display::decode_event`] turns a received [`Message`] into an
/// [`display::Event`]. The app polls that channel; the compositor forwards only
/// events for the focused surface.
///
/// ## Rendering
///
/// [`display::Canvas`] is the userspace software blitter ([`display::font`] is
/// a 5x7 bitmap font): apps and the compositor draw into the same shared-buffer
/// mapping the kernel handed out, so compositing an app's window is a plain
/// memory copy from the app buffer into the screen buffer. The tiny-skia-like
/// in-kernel renderer cannot be linked from ring 3, which is why this path is a
/// simple blitter; the XUI/tiny-skia toolkit is the S4 follow-up.
///
/// ## Shell extensions (issue #167, S5.0)
///
/// LazyShell (S5) is one more display client, so the compositor gains an
/// append-only set of methods and one-way events; older clients and older
/// compositors keep working because unknown TLV fields and methods are ignored:
///
/// * `CreateSurface` gains a `role`: [`display::wire::ROLE_WINDOW`] (the
///   default when the field is absent) or [`display::wire::ROLE_DESKTOP`]. A desktop surface paints at the
///   bottom of the z-order, above the compositor background and below every
///   window, with no chrome and no taskbar entry; creating a second one
///   replaces the first.
/// * `ListSurfaces` replies with one [`SurfaceInfo`] row per surface (id,
///   title, geometry, minimized, focused); `GetWorkArea` replies with the
///   rectangle available to windows (the fallback taskbar is excluded while it
///   is visible), and `GetTheme` reports the chrome [`Theme`] so the shell can
///   match xuid's palette.
/// * `Subscribe(role, events)` transfers the shell's event endpoint. The
///   compositor sends one-way [`ShellEvent`]s there: `SurfaceChanged` on
///   create/destroy/move/minimize/restore/title, `FocusChanged`, and
///   `StartMenu` when the global `Ctrl+Esc`/`Super` hotkey fires. The role
///   `"shell"` also hides the built-in taskbar; the compositor stays usable
///   with no shell attached.
/// * The global hotkeys live in the compositor: `Alt+Tab` shows a centered
///   overlay, cycles on repeated Tab, and commits on Alt release; `Alt+F4`
///   sends `WindowClose` to the focused surface; `Escape` cancels a drag & drop.
pub mod display;

/// The input service protocol (`docs/input-plan.md`): `os.lazy.input.v1` for
/// clients and `os.lazy.input.shell.v1` for the compositor, plus the
/// compositor's [`input::ShellLink`].
pub mod input;

/// Wire shapes for `mimed` (issue #158): MIME/handler registry and app launch
/// records, plus the `system/events/open/<app>` interim launch event.
pub mod mime;

// ---------------------------------------------------------------------------
// clipboard: the per-session clipboard service (issue #115)
// ---------------------------------------------------------------------------

/// Client and wire shapes for `clipboardd`, the per-session clipboard service
/// (`docs/platform-plan.md` section 4.5, `docs/messenger.md` section 19).
///
/// An interaction is a typed offer plus a request:
///
/// * [`Client::copy`] (or the lower-level [`offer_request`]) publishes one or
///   more MIME payloads for the caller's session; the service answers with a
///   **token**;
/// * [`Client::paste`] / [`Client::paste_token`] request a payload by token
///   and MIME. A **lazy** offer sends only its MIME list; when a paste finally
///   happens the service calls the owner's [`method::SERIALIZE`] on the
///   endpoint registered under the offer's sink and forwards the bytes, so the
///   owning app materializes the data on demand;
/// * every offer announces itself on the retained per-session topic
///   `session/<id>/clipboard/changed` (declared in `idl/clipboard.midl`);
///   [`wire::subscribe_session_clipboard_changed`](clipboard::wire) attaches a
///   subscriber so paste UIs refresh without polling.
///
/// # Buffer handle
///
/// There is no userspace shared-buffer syscall yet (the kernel object and its
/// `SHARE_ONLY` rule live in `kernel/src/ipc/shared.rs`; `keyd` documents the
/// same gap), so a paste's [`BufferHandle`] currently carries the bytes inside
/// the reply parcel, bounded by the service's [`MAX_DATA`] on the eager path.
/// The wire shape is what a mapped `SHARE_ONLY` buffer will carry once the op
/// lands.
///
/// # Policy
///
/// `Offer` and `Request` parcels put the *pseudo-interface* ids
/// [`WRITE_INTERFACE`] (`os.lazy.clipboard.write.v1`) and [`READ_INTERFACE`]
/// (`os.lazy.clipboard.read.v1`) in their parcel header. The kernel's
/// `ipc::authorize` hook derives `(interface_id, method)` from that header on
/// every outbound call, so an ACL rule keyed on `clipboard.write` /
/// `clipboard.read` gates offering and pasting, and a denial is recorded in the
/// kernel audit ring before the service ever sees the parcel. On top of that
/// the service enforces the **session scope**: a token offered by session A is
/// refused (and logged) for session B.
pub mod clipboard;

// ---------------------------------------------------------------------------
// confd: the configuration registry (issue #260)
// ---------------------------------------------------------------------------

/// Client and wire shapes for `confd`, the configuration registry
/// (`docs/confd-plan.md` v1).
///
/// The interface is generated from [`idl/confd.midl`](../../idl/confd.midl) into
/// `messenger-generated`; this module wraps it with a typed [`confd::Value`]
/// conversion, a [`Client`] that resolves-and-retries at boot, and the
/// `system/confd/changed/<path>` change-payload codec.
///
/// Only `sys/` paths are announced: the kernel topic policy cannot express the
/// "`user/<uid>` is owner-only" rule, so publishing user changes would leak
/// them to every subscriber (issue #260's follow-up). A subscriber therefore
/// re-reads after a change and must not assume every write produces an event.
pub mod confd;

/// The time-of-day service `timed` (issue #369): the generated
/// `os.lazy.timed.v1` stubs and a blocking [`timed::Client`].
pub mod timed;

/// The device manager `devd` (issue #497): the generated `os.lazy.devd.v1`
/// stubs and a blocking [`devd::Client`].
pub mod devd;

/// The network mount service `mountd` (docs/smb-plan.md §3.4): the generated
/// `os.lazy.mount.v1` stubs and its service name.
pub mod mount;

// ---------------------------------------------------------------------------
// Audio (docs/driver-plan.md D6)
// ---------------------------------------------------------------------------

/// The `os.lazy.audio.v1` client: streams, the shared ring and the transport
/// controls of the `sndd` driver.
pub mod audio;

// ---------------------------------------------------------------------------
// Networking (docs/networking-plan.md N1)
// ---------------------------------------------------------------------------

/// The `os.lazy.net.nic.v1` client: control calls on the `netdrv` driver and
/// the client's side of its frame rings.
pub mod net;

/// The `os.lazy.net.stack.v1` client: addresses, routes, statistics and ping
/// against the `netd` stack service.
pub mod netstack;

/// The `os.lazy.net.socket.v1` client: TCP and UDP sockets served by `netd`.
pub mod netsock;

/// `TcpStream`, `TcpListener` and `UdpSocket` over [`netsock`], named after
/// `std::net`.
pub mod netstd;

/// Nap one PIT tick's worth between retries ([`crate::sys::nap`], a real sleep;
/// this used to park on a throwaway channel pair, since userspace had no
/// sleep call).
pub fn park_tick() {
    crate::sys::nap();
}

/// Client and wire shapes for `pkgd`, the application package manager
/// (`docs/packages.md`, `idl/pkgd.midl`).
pub mod pkgd;
