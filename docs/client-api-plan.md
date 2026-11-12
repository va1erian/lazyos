# Client API plan: generated Messenger clients and one xui app shape

A review of the API every LazyOS client program uses (the Messenger runtime,
the code `midlc` generates, and the `xui-app` layer desktop apps build on),
with a plan to make apps smaller, make every client behave the same way, and
make the IDL the only place a protocol is written.

Status: in progress (2026-11-11). Built: C1 (the client runtime, section
3.4), C2a (the `service`, `errors`, `scope` and `deadline` syntax, their
generated constants and the per-interface crate layout, sections 3.1 and
3.6) and C2b (the generated Rust client, section 3.2; `user::messenger::timed`
and the xui `platform/notify.rs` already use it) and X1 (`xui_app::app(..)`,
section 4.1: `.opens` takes a path-to-message function and `.drops` a
paths-to-message one, since the MIME types an app opens live in its
manifest; `lrplay` still starts lazily on its own) and the resend and deadline policy (`retry=`, `deadline=never`, `default`, section 3.2). The rest is not.

Related: [messenger.md](messenger.md) (sections 11 and 15),
[midl.md](midl.md), [lazyrad-messenger-plan.md](lazyrad-messenger-plan.md)
(the Rhai side, which already has what this plan gives Rust).

## 1. Findings

### 1.1 `midlc` generates codecs, not clients or servers

For each interface, `libs/generated/src/lib.rs` (one file of 27,592 lines)
holds `INTERFACE_ID`, `METHOD_*` ids, an `XxxArgs`/`XxxReply` struct with
`encode_*`/`decode_*` for every method, object kinds and typed topic helpers.
It has no client stub, no server trait and no dispatch: the only traits in it
are `topics::Publish` and `topics::Subscribe`.

Rhai already gets more than Rust: `midlc_rhai.py` writes one function per
method (`sys::confd::get("sys/ui/theme")`) over the generic `msg` module, so a
script never touches the wire. A Rust program writes that layer by hand,
every time.

### 1.2 So every client and server hand-writes the framing

The codecs are generated; the code around them is not, and it is copied:

| Pattern | Copies |
|---|---|
| `Header { version, flags, interface_id, method, .. }` built by hand | ~206 sites outside the generated crate; `fn parcel(` defined in 26 modules |
| Reply scanned for `Kind::Error && id == ERROR_FIELD` | 15 (28 `Kind::Error` scans); the generated `errors::find`/`find_code`/`is_error` have **zero** callers |
| `fn error_reply` | 16 |
| `match method { wire::METHOD_X => decode, act, encode }` dispatch arms | 469 |
| Registered service name as a string constant (`"os.lazy.confd"`) | ~75 definitions; `os.lazy.init` 7 times, `os.lazy.messenger.topics` 6, `os.lazy.confd` 5 |
| `CONNECT_TICKS`-style resolve timeouts, tuned separately | 16 |
| `const fn fnv1a64` | 6 copies (kernel, clipboard twice, devinspect, pkgstore, playground) |
| Constants the IDL does not declare, copied across crates | confd `KIND_*` and `CONFD_*` codes, the clipboard scope ids |

`lazyos_sys::msg::parcel::request`, the one shared request builder, has a
single caller (`libs/rhai-lazy/src/msg/gate.rs`).

Example, the `timed` server (`user/src/bin/timed/handler.rs`): of its 111
lines about 40 are interface checks, decode/encode calls, `map_err` and the
reply parcel. The xui clipboard client (`xui-app/src/platform/clipboard.rs`)
carries its own `fnv1a64`, its own service name, raw errnos (`Err(-22)`) and
its own timeouts.

### 1.3 Two parallel client stacks, and more beside them

The same service is wrapped once for native programs and again for xui apps,
each with its own connect, retry, cache, timeout and error type:

| Wrapper | Connect / retry | Cache | Errors |
|---|---|---|---|
| `user::messenger::<svc>::Client` (~22) | `connect_retry(n)` + `park_tick` | none | `messenger::Error` with per-service variants |
| `xui_app::platform::messenger::Service` | 10 s loop with `sleep_millis(10)` | thread-local, evicted on peer death | `i64` or `CallError`, seven `call_*` variants |
| `lazyrad-os::transport::MessengerTransport` | a copy of the same loop | its own `RefCell<Vec<..>>` | `Failure` |
| `xui_app::fabric::Broker` | resolve on first use | release on `ENOENT`/`EPIPE` | `Topics::Offline` |
| `display.rs`, `input.rs`, `input/grab.rs`, `platform/audio.rs` | raw `msg_resolve` | audio has its own `RESOLVED` | ad hoc |

Services wrapped more than twice: `confd` (user `Client`, xui `ConfdStore`,
`shell/services.rs`, `term/conf.rs`, LazyWeb), `init` (six wrappers), topics
(eight subscription clients, three of them inside `xui-app`: the bell-driven
`fabric::Subscriber`, used only by the Playground; `platform::topic_feed`,
which polls `NextEvent` with an expired deadline; and `shell/failures.rs`;
all three are one mechanism since C5, section 3.5).
Seven separate `Transport` traits exist as test seams (`audioclient`,
`trayclient`, `lazyrad-os`, `fabric/subscribe`, `webd-client`, clipboard,
`svcpolicy`).

Two server runtimes (`user::messenger::Server`, blocking, and
`xui-app/src/server.rs`, polled); only three daemons use `Server::serve`,
the rest write their own receive loop.

### 1.4 What the IDL cannot say yet

Facts that live in prose or in hand constants because MIDL has no syntax for
them:

* **The registry name** a service is served under (`os.lazy.confd` for
  `os.lazy.confd.v1`) is written only in doc comments.
* **ACL scope interfaces** (clipboard's `os.lazy.clipboard.write.v1` /
  `.read.v1`) are hashed by hand in two crates.
* **Service error codes** (`CONFD_NOT_FOUND = 1001`, ...) are hand constants.
* **Discriminants** declared as `U32` instead of an `enum` (confd
  `Value.kind`).
* **A call's deadline**: every client picks its own (clipboard 0.5 s and 5 s,
  others 1 to 10 s), so the same call can time out in one app and not another.
* **Topics** published with no declaration: `system/events/security/denial`,
  `system/events/audit/overflow`, `system/events/selftest/forbidden`,
  `demo/waitany`, `topics/depth`; plus the interim per-service router
  (`os.lazy.local.topics.v1`, ids `*b"os.topic"`, hand-written methods and
  fields, still used by `init` and `healthd`), the clipboard owner scope
  (`*b"os.owner"`) and the `messenger_async` health parcel.

### 1.5 `xui-app`: seven ways to start an app

Bins in `xui-app/src/bin` total 28,027 lines; much of each `main` is the same
set-up written differently:

* **Launch paths:** `launch::run` (26 files), `themed::run_themed` (editor,
  term, docs, mail), raw `run_app` plus a manual theme (archiver, pdf, video,
  writer, LazyWeb, shell), `xui_core::app` directly, `Resident::connect`,
  `LazyOSBackend::new_client`, and LazyRAD's own. `pdf.rs` and `video.rs`
  are the same 50 lines with different names.
* **The theme step is redundant everywhere:** `LazyOSBackend::system_theme`
  already returns the desktop theme (`backend/handlers.rs:22`) and xui's
  `run_app` applies it, so `themed.rs` and six manual `set_theme` blocks do
  nothing.
* **Dropped files:** the same "outbox" (`Rc<RefCell<Option<Proxy>>>` filled
  after `run_app` so a drop handler can reach the UI) is copied in pdf, video,
  term and archiver; NES shows it is unnecessary by registering the handler
  inside `make` with `ui.proxy()`.
* **Exit:** `-> !`, `ExitCode::from(finish(..))`, a hand `match` that skips
  `finish` (mail), `process::exit(1)` (term).
* **Evidence:** about 296 `println!` markers in bins and crates against about
  110 trace-macro uses, plus `Report` enums mapped to prints (calc, about,
  golf, LazySky) and `Host.log` closures; three panic hooks.
* **Worker to UI thread:** four mechanisms (`Proxy::send`; mpsc polled from a
  timer; an `Arc<Mutex<Option<..>>>` slot polled from a timer; custom wake
  callbacks). `archiver/src/job.rs:4` and `pdfview/src/worker.rs:5` said the
  backend has no cross-thread waker; `backend/wakeup.rs` is one. (X3 replaced
  all four with `xui_app::work`, section 4.3.)
* **Per-app services re-derived:** settings (confd keys built with `format!`
  in six places, a TOML file in LazySky, environment variables in Mail, a
  `recent.txt` in NES); the app data directory three ways; `$HOME` resolved
  by hand in about nine places although `platform::dirs::default_dir` exists;
  four copies of a `Host { start_dir, file_system, log }` seam (archiver,
  pdfview, videoview, writer) plus LazyWeb's; the same `FileDialog` builder
  chain in eight places; recent files only in NES; no shared error dialog.
* **Shared code duplicated:** atomic write (temp file plus rename) in 16
  files; three image decoders (`image`, `png`, `zune-jpeg`); Paint's own
  `inflate.rs` and `zip.rs` (746 lines) beside `crates/archive`; plain HTTP in
  `src/net/{http,web}.rs` beside `crates/https`; the CA bundle lookup in
  `lazysky.rs` instead of `https`.

## 2. Principles

1. **The IDL is the protocol.** Everything on the wire (names, ids, scopes,
   errors, deadlines, topics) is declared in `idl/` and generated. A
   hand-built `Header`, a hand scan for the error field or a hand `match` on
   method ids outside the generated crate and the two transports is a lint
   failure.
2. **One client per interface, for every runtime.** Generated clients are
   generic over a small transport trait, so `user` (no_std), `xui-app` and
   `lazyrad-os` (std over `lazyos-sys`) and host tests share them. A
   hand-written module exists only where it adds meaning (clipboard's
   in-process fallback, confd's `Value` conversion), never framing.
3. **One way to do each thing in an app**: start, open a file, run work off
   the UI thread, store a setting, report evidence. The template uses it;
   the old ways are deleted, not deprecated (no compatibility shims).
4. **Identical behaviour by construction.** Timeouts, retries, error text and
   reconnect come from the IDL and one runtime, not from each caller.

## 3. Messenger: what `midlc` should generate

### 3.1 IDL additions

```text
/// Served under this registry name.
service "os.lazy.confd" implements os.lazy.confd.v1;

interface os.lazy.confd.v1 {
    /// Service errors; replies carry them in the standard error field.
    errors { NotFound = 1001, BadPath = 1002, TooLarge = 1003, Denied = 1004 }

    enum Kind { Bool, I64, U64, Str, Bytes }

    /// A default deadline every generated client uses; callers may shorten it.
    method Get(path: String = 1) -> (value: Option<Value> = 1) deadline=500ms;
}

interface os.lazy.clipboard.v1 {
    /// Travels on an ACL scope interface instead of this interface's id.
    method Offer(..) -> (token: U64 = 1) scope "os.lazy.clipboard.write.v1";
}
```

As built (C2a): the generated enum is `ServiceError` (every interface module
already imports the codec's `libmessenger::Error`), a `service` may be a family
(`"os.lazy.net.nic/{ifname}"`, giving `SERVICE_INSTANCE_PREFIX`), an interface
has at most one singleton name and one family, and a method without `deadline`
gets no constant. Syntax and rules are in [midl.md](midl.md).

Each addition replaces a family of hand constants in section 1.4: `service`
gives `SERVICE_NAME` (and the manifest gets the name-to-interface map the
registry and `devtool` checks can read), `errors` gives a typed error enum,
`scope` gives `SCOPE_ID` (and deletes every `fnv1a64` copy outside the
kernel and midlc), `deadline` gives every client the same bound and lets
`tools/xui/check_call_bounds.py` read it instead of a heuristic.

### 3.2 Generated client

```rust
// generated: messenger_generated::confd (feature "confd-client")
pub struct Client<T: Transport> { conn: Connection<T> }

impl<T: Transport> Client<T> {
    /// Resolves `os.lazy.confd`, waiting for registration up to the runtime's bound.
    pub fn connect(t: T) -> Result<Self, CallError<Error>>;
    pub fn get(&self, path: &str) -> Result<Option<Value>, CallError<Error>>;
    pub fn set(&self, path: &str, value: &Value) -> Result<(), CallError<Error>>;
    /// Same call with the caller's deadline instead of the IDL's.
    pub fn with_deadline(&self, deadline: Deadline) -> Self;
}
```

Rules: arguments are flattened into parameters; a one-field reply returns the
field, a multi-field reply its struct; object fields take `Buffer` /
`Channel<I>` values and move them; the remote error is decoded with the
generated `errors` module and mapped to the interface's `errors` enum.
`oneway` methods return `Result<(), CallError<_>>` without waiting. A
`call_async` form returns a `Pending<R>` whose completion a UI hands to its
`Proxy` (section 4.3), so no UI thread ever blocks on a service.

As built (C2b; the user-facing description is [midl.md](midl.md), "Generated
client"): the client is `Client<T: Transport>` in the interface's module, behind
`<interface>_client`, and its error alias is `ClientError`
(`CallError<ServiceError>`, or `CallError` without `errors`). Where the runtime
forced a change to the sketch above:

* **Deadlines.** `with_deadline(&self, ..) -> Self` cannot exist (a
  `Connection` is not `Clone`: it owns the cached handle), so the one override
  is per call, `<method>_within(.., Deadline)`. There is no implicit bound: a
  method states `deadline=` (or `never`) or its interface a `default`, and
  `midlc` refuses a client method that resolves none (the 5 s
  `DEFAULT_DEADLINE` of C2b is deleted; see the policy note below).
* **`_begin` is not built.** `Transport` has no asynchronous call; the xui
  lane (`platform::messenger::call_async` over `backend::calls::begin`) is
  outside it.
  Section 4.5's `Pending<R>` needs a `begin`/`complete` pair on the transport
  first; until then a UI-thread caller keeps its existing lane for those calls.
* **Families.** A family member (`os.lazy.net.nic/eth0`) is a
  `Connection::named(.., format!("{prefix}{instance}"))` handed to
  `Client::from_connection`; the connection owns its name.
* **Copies.** The generated `XxxArgs` structs own their fields, so a borrowed
  argument (`&str`, `&[u8]`) is copied once before it is encoded; a large
  `Bytes` argument pays one extra `memcpy` (use a `Buffer`, which is shared).
* **Mapping.** `Service(ServiceError)` carries the code only; a service's
  sentence and detail survive on an undeclared code (`Remote`). A malformed
  reply is `Transport(EBADMSG)`, an unencodable request `Transport(EINVAL)`.
* **Which interfaces.** No client for a kernel scope (`KERNEL_SCOPES`) or an
  interface no `service` serves. `Connection::send` now takes the objects of a
  `oneway` request, and `Mock` records them.

As built (C3, the xui half): every client in `xui-app` and `lazyrad-os`'s
failure report goes through the generated `wire::Client<Observed>`, and
`Service`'s `call`, `call_moving` and `send_oneway` are gone. One helper holds
them: `xui_app::platform::messenger::client(wire::Client::connect)` (or
`try_connect`, for a caller that must not wait for the service) returns this
thread's `Rc<Client>`, made on first use and kept for the thread, so no module
keeps a cache of its own; `instance_client(prefix, instance,
wire::Client::from_connection)` does the same for a family member. What stays
hand-framed, and why:

* `platform::messenger::call_async`, the pkgd install's non-parking lane (no
  generated `_begin` yet, see above).
* The raw transports of libraries whose seam is method id, body and objects:
  `libs/audioclient` (`platform::audio`), `libs/trayclient` (`tray`) and
  `xui-webd-client` (`platform::webd`, one seam over the light and the full
  interface). They hold a `Connection` of their own (`Connection::named`) and
  call `Connection::call`/`send`; moving them to generated clients is a change
  of those libraries' `Transport` traits, not of the xui side.
* The topic subscriptions: C5 (section 3.5). `platform::topic_feed`, the
  failures poller and the LazyRAD handoff's `NextEvent` poll are gone;
  `fabric::Follow` replaces them.
* The LazyRAD handoff's `Transport` seam (`lazyrad-os/src/transport.rs`):
  its 426 lines of tests script that trait, and the crate does not build on
  this tree's xui pin (its `lazyrad-ide` git dependency is older than the xui
  revision), so it keeps the seam over `instance_client("", name,
  identity)` until it can be built and migrated onto `Mock`.

`tools/xui/check_call_bounds.py` polices the new form: a file that builds a
generated client of an interface may not call a method the `.midl` resolves
to `deadline=never` (it would park the UI thread); use `<method>_within` with
a bound.

As built (C3, the rest of the native clients and the library seams):

* **`net` (NIC), `netstack`, `netsock`, `netstd`.** `net::Client` and
  `netstack::Client` are `wire::Client<Native>`; the NIC's ring attach
  (`net::attach`, `attach_notifying`) and the stack's parked calls
  (`netstack::ping`, `resolve`, `lookup_host`, which bound a call by the
  caller's `timeout_ms` plus a second) are free functions. `netsock::Client`
  stays a thin struct over `wire::Client<Native>` because it is a domain
  adapter: `Addr` for the wire's byte vector and port, the 10 ms to 60 s timeout
  clamp, the 16 KiB chunk cap, and the parked-call bound. `netstd` and every
  tool get `CallError`; `netsock::{is_errno, describe}` replace three copies.
* **`wifi`.** `Radio` and `Client` are the generated clients (`connect_radio`,
  `scan_results`'s page loop). `SetKey` and `AddNetwork` carry a secret, so
  `wifi::set_key` and `wifi::add_network` keep a buffer sized up front and
  wiped, framing on the connection (`call_secret`); that is the one hand-framed
  client call left here, and the reason is security, not convenience. The radio
  interface's `default deadline` is the 3 s `wlanmd` gave a driver.
* **`display`, `input`.** `display::Client` wraps `wire::Client<Nested>` (and
  the cursor scope's client) with `Rect` adapters; `Nested`, the transport that
  sets `ALLOW_NESTED` (an app's event poll must not trip the kernel's
  per-channel cycle check while a `Commit` is in flight), moved from the
  clipboard to `messenger::client`. `input::ShellLink` and `KeySession` keep
  `inputd`'s private shell channel as a `Held` transport (a handle the task
  already owns, released with the connection only when it came from name
  resolution) under the generated clients, with the compositor's 200 ms bound
  passed per call (`_within`).
* **`audioclient`.** The `Transport` trait is gone: `Client` and `MixerControl`
  are the generated clients, and `PlaybackStream<T: Transport, R: Runtime>`
  keeps the ring arithmetic, with `Runtime` reduced to what is not framing
  (rings, clock, sleep). `user::audio`, `xui_app::platform::audio`, the `audiod`
  card client, `beep`, `mixer`, `modplay`, Volume, NES and the terminal bell use
  it; the stream tests run the real mixer engine behind a `libmessenger`
  `Transport`. The mixer control interface declares the 2 s bound xui gave it.
* **`trayclient`.** `Tray<T: Transport, C: Channels>` calls through
  `wire::Client`; `Channels` is only the channel pair source. Its tests run on
  `Mock`; the tray interface declares the 1 s bound. A call resolves the shell
  with one attempt (`reach`), as before.
* **Still hand-framed, and why.** `webd` (`platform::webd`, `xui-webd-client`):
  one `Transport` seam over two interfaces (`webd.light` and `webd.view`, with a
  scoped `CreateView`), shared memory, an event channel and an outbox that
  resends the oneway messages that must arrive; a generated client per
  interface cannot be that seam without losing the outbox and the view-slot
  checks. The hostile-input probes (`netctl probe/sockprobe`, `nicctl probe`,
  `beep probe`) send what no generated client would (foreign interface ids,
  missing objects, requests the kernel drops), through `Connection::call` or the
  endpoint, each with a comment saying so. `audio_events` publishes a topic
  (C5).

As built (resend and deadline policy, after C3): the policy lives in the IDL.
`deadline=<n>ms|<n>s|never`, `retry=safe|never` and an interface-level
`default deadline=.. retry=..;` line are in [midl.md](midl.md) ("`deadline`,
`retry` and `default`"); generated as `METHOD_<X>_DEADLINE: Deadline` (replacing
`_DEADLINE_MS`) and `METHOD_<X>_RETRY: Retry`. Findings that shaped it:

* **The kernel can tell never-queued from queued-then-died, the syscall
  cannot.** `begin_call` fails `EPIPE` for a closed peer endpoint and
  `ENOENT` for a stale handle before it queues anything; `await_reply` ends
  `PeerDied` (also `EPIPE`) for a peer that took the request and died. One
  `OP_CALL` reports both as `EPIPE`. The native and std transports therefore
  call `OP_CALL_BEGIN` then `OP_CALL_AWAIT` for a `retry=never` method (one
  extra syscall, no kernel change; `Transport::call` takes the policy, and a
  `retry=safe` call keeps the single `OP_CALL`) and fail with `CallFailure { errno, delivered }`;
  `Connection::call` resends whatever the policy when `delivered` is false.
  A transport that cannot tell says `delivered`.
* `retry=never` is the default; it returns the error, drops the handle and the
  next call reconnects. `retry=safe` is marked on reads (`Get`, `List`,
  `Status`, `Info`, `Ping`, `Now`, `Lookup`, `Resolve`, ...) and set-to-a-value
  writes (confd `Set`/`Delete`, `SetZone`, display `Set*`). Everything that
  launches, mounts, installs, creates, connects, logs in, offers, requests or
  notifies stays `never`.
* `deadline=never` is on the methods all of whose callers waited for ever
  (accounts `Authenticate`, keyd `Verify`/`Provision`/`WifiPmk`, pkgd
  `InstallApproved`/`InstallDebug`); the `_within(.., Deadline::Never)` at
  those call sites is gone. Methods with a bounded caller (pkgd `Inspect`,
  `Install`, `Remove`, `Installed`, init `Launch`, mimed `Open`, elevd `Request`,
  accounts `Lookup`) keep their IDL bound and the `_within(.., Never)` at the
  slow caller (`pkgctl`, `elevd`, `mimed`, a worker thread).
* The three raw seams (`platform::audio`, `platform::webd`, `tray`) pass
  `Retry::Never`: they frame methods the IDL policy cannot reach, so only a
  never-queued request is resent.
* Every interface with a client has a `default deadline=` (the bound its
  undeclared methods already waited: 5 s, or the interface's commonest).

### 3.3 Generated server

```rust
// generated: messenger_generated::timed (feature "timed-server")
pub trait Timed {
    fn now(&mut self, cx: &Caller) -> Result<NowReply, Fault<Error>>;
    fn set_zone(&mut self, cx: &Caller, name: String) -> Result<(), Fault<Error>>;
    // ... one method per IDL method
}
/// Checks the interface (or scope) id, decodes, calls, encodes the reply or
/// the standard error field; an unknown method is `ENOSYS`, a bad body `EINVAL`.
pub fn dispatch<S: Timed>(server: &mut S, message: &Message) -> Parcel;
```

`Caller` is the kernel-stamped credential (uid, caps, label, session), so
authorization stays in the handler but never parses anything. This removes
the 469 dispatch arms, the 16 `error_reply` builders and the per-module
`fn parcel`; `timed`'s handler shrinks to its six method bodies.

As built (C4): the trait is `Server` in each interface module, behind the
feature `<iface>_server`, over the runtime seam `libmessenger::server`, so the
generated crate still depends on `libmessenger` alone and one `dispatch`
serves both runtimes:

* `Incoming` (what a received message offers: `interface_id`, `method`,
  `caller`, `decode`) is implemented by `user::messenger::Message` and
  `xui_app::server::Request`; a host test implements it for a mock.
* `Caller { uid, gid, caps, label_id, session, sender }` is what the kernel
  stamped when the message was queued; `sender` is the task slot, for
  addressing a per-client table only. Authorization stays in the handler; no
  `requires` attribute was added, because the checks that repeat (`CAP_SETUID`
  and unlabelled, `elevd` by identity, root only) take different shapes per
  daemon and a declared form would have hidden what they compare.
* `Fault { code, message, domain, hint, docs }` carries the errno or declared
  code, the sentence and the optional detail record. `From<Errno>`; each
  interface with `errors` gets `From<ServiceError>` (the doc line of the error
  is its sentence); `user` adds `From<messenger::Error>` so `?` on a Messenger
  call answers what the hand-written `error_reply` did.
* A method returns `Result<T, Fault>`: `()` for no reply field, the field's
  type for one, the `XxxReply` struct for several (the shapes the client
  returns). A `oneway` method returns `()`. **Every method is required** (C4b):
  there is no default body, so a daemon that leaves one out does not compile.
  A daemon that serves part of an interface refuses the rest on purpose
  (`Fault::unserved()`, `ENOSYS`), and an interface that carries events in
  both directions (wifi hw, nic, input, input shell, init app, echo) needs a
  one-line empty body for the events only a client receives.
* `dispatch(&mut S, &impl Incoming) -> Parcel` answers `EINVAL` for another
  interface (or a scoped method on the wrong scope id), `ENOSYS` for an
  undeclared method, `EINVAL` for a body that does not decode (objects stay
  with the message and close when it drops; a decoded request's objects are
  the handler's to close on a refusal) and `EIO` for a reply that does not
  encode. A runtime sends the parcel only when the message was a call (a
  `oneway` method yields an empty parcel nobody sends).
* Interfaces that are kernel ACL scopes or policy loaders, and the
  kernel-served registry, get no server. Tests:
  `libs/generated/tests/server.rs` (mock message, faults, objects, scopes,
  oneway, client to server through `Mock`), `tools/midlc/test_midlc_server.py`.

Migrated (native `user`): `timed`, `mimed`, `pkgd`, `confd`, `mountd`,
`accountsd`, `logd`, `sysmond`, `devd`, `gpud`, `elevd`, `keyd` (both of its
interfaces), `clipboardd`, `sndd`, `usbd` (audio function), `netdrv`, `wlanmd`
(the wifi interface), the radio runtime (`user::wifi`: hw and nic) and, in C4b,
`healthd`, `logind` (console and graphical), `inputd` (both interfaces),
`netd` (stack and socket) and `init` (control and app lifecycle); std
(`xui-app`): `printd`, the shell, notify and tray services.

As built (C4b), replies that come later: the generated `dispatch` returns a
`Parcel`, and a method that answers when something else happens (a `Stop` when
its targets exit, a `Home` when its helper does, a parked `netd` socket call or
ping) keeps the deferral in the daemon over the generated decode and encode.
The handler is built per request with the message's transaction
(`Handler { txn, held, .. }`, or `netd`'s `Netd::call`); a deferring method
hands the transaction to the table that answers it, sets `held` and returns
`Fault::unserved()`, and the loop sends the parcel only when `held` is clear.
The later answer is encoded with the generated `encode_<m>_reply` and
`libmessenger::server::fault_reply` (netd's `Done` enum is the typed result a
parked call finishes with). No generator change: a held reply handle in the
trait would have made every handler pay for what three daemons need, and the
flag is the only thing the loop has to look at. `logind`'s `After` is the same
idea in the other direction: the method leaves what must follow the reply in a
field the loop reads.

Not migrated, and why: `xuid` (the display and cursor interfaces, the trusted
prompt's deferred reply and the compositor's own shell checks are one large
request path whose helpers decode the message themselves; its ~25 methods are
the next slice), `audiod` and `libs/audiomix` (the pure `Outcome` layer its host
tests drive decodes bodies itself; it moves with its 479 lines of tests) and
`webd` (the quota and slot checks sit between decode and act; a generated
dispatch must not move them).

### 3.4 One transport, one connection, one error type

In `libmessenger` (no_std):

```rust
pub trait Transport {
    fn resolve(&self, name: &str, until: Deadline) -> Result<Handle, Errno>;
    fn call(&self, h: Handle, p: Parcel, until: Deadline) -> Result<Parcel, Errno>;
    fn send(&self, h: Handle, p: Parcel) -> Result<(), Errno>;
}
pub enum CallError<E> { Transport(Errno), Remote { code: u32, message: String, detail: Detail }, Service(E) }
```

Two implementations: `user::messenger` (native) and one std implementation in
`lazyos-sys` that `xui-app` and `lazyrad-os` share, plus a `Mock` in
`libmessenger` for host tests that records calls and scripts replies. That
deletes the seven test-seam traits.

`Connection` (generic, in `libmessenger`) owns resolve, the name cache,
eviction on `EPIPE`/`ENOENT` and one reconnect (a resend only as the method's `retry` allows, section 3.2). **Built in C1** (module
`libmessenger::client`; transports `user::messenger::client::Native` and
`lazyos_sys::client::Std`; `Mock` behind the `mock` feature). Each transport
states the flags its calls carry: native calls send none, so the kernel still
refuses one that would close a wait cycle, and the std runtime's UI loops send
`ALLOW_NESTED` as before.

Waiting for a service that has not registered yet blocks on the registry, it
does not sleep. **Built in C1b:** the registry method `WaitFor(name)`
(`idl/registry.midl`, op `WAIT_FOR` 27, frozen ABI 2) is a `Resolve` whose
caller the kernel parks until a `Register` of that name or the call's
deadline (`ETIMEDOUT`), and both transports' `resolve` call it with the
10 s connect bound; `Deadline::Poll` stays one plain `Resolve`. The sleep
and `park_tick` loops are gone, so the "never poll on a timer" rule holds
here too. A retained topic was not chosen: it would need a subscription, a
bell and a race-free "already registered" read, where one parked syscall
re-checks the table itself. The kernel side (`kernel/src/ipc/registry/
wait.rs`) takes a ticket in the registry's waiter table before the first
look (no lost wake-up), wakes only the waiters of the registered name after
the registry lock is released, re-applies the caller's name policy when the
name appears, and bounds waiters at one per task, 16 per process and 512 in all, 128 of them reserved for platform tasks, i.e. no label and no login session (a user's own programs share the open part; many same-uid processes can still fill it) (`EAGAIN`, on which both transports fall back to plain `Resolve` retries, the only sleep left); a signal ends the wait with `EINTR` and the transports run an interrupt hook (std: `sched_yield`, because an `int 0x80` return never lays out a Linux handler frame) and re-issue it with the same deadline (`libmessenger::client::resolve_waiting`). A name that never registers now ends a connect with `ETIMEDOUT`, where the polling runtimes ended with `ENOENT` (the generic text in `user::messenger::Error` covers both).
`Errno` is a named newtype
(`Errno::INVAL`), so `Err(-22)` disappears; `CallError` implements `Display`
with the friendly text section 12 of `messenger.md` describes.

### 3.5 Topics: one subscriber

The generated `subscribe_<topic>` returns a typed
`Subscription<OfferMeta>` driven by the bell (the design `fabric::Subscriber`
already has, which is right and under-used). `platform::topic_feed`, the
`shell/failures.rs` poller, LazyRAD's handoff poller and
`user::messenger::router` (after moving `init` and `healthd` to the central
broker) are deleted. The undeclared topics in section 1.4 get declarations
or are removed.

As built (C5):

* **The typed stream is in the generated crate and knows no transport.**
  `topics::Subscribe::Subscription` is a transport handle that implements
  `topics::Events` (`next_event()`: one queued `RawEvent {topic, retained,
  publisher, payload}`, never waiting). `subscribe_<topic>(..)` returns
  `topics::Subscription<Handle, Payload>`, whose `next()` decodes the declared
  payload into `topics::Event<Payload> {topic, retained, publisher, value}`
  (`Err(NextError::Decode)` consumes the bad event; `Transport` is the
  transport's own error). `filter_<topic>(..)` builds a subscribe filter beside
  `name_<topic>(..)`, so no consumer types a topic string a `.midl` declares.
  The wildcard state topics (`system/events/service/+`, `system/health/+`,
  login `session/+` and `end`, `system/net/+/{link,addr,wifi/state}`) are
  `qos=buffered` and the helpers take the broker's deepest queue: a `latest`
  slot would keep one sibling's value and lose the others'.
* **Native: one client.** `user::central::Bus` implements `Publish` and
  `Subscribe` (a `central::Subscription` is the `Events` handle).
  `central::Feed<T>` is the service-side recipe: the generated helper, one
  registry lookup (never a wait), every call bounded, the doorbell, the replay
  read of a new stream, and a reconnect; a loop parks on `Feed::bell()` beside
  its endpoints (`init`, `healthd`, `logd`), a service that reads at moments
  of its own asks the bell (`Feed::drain_rung`: a local queue, never the
  broker). `central::Publisher` is what `init`, `healthd` and `logind`
  publish through: bounded, with a cooldown after a timeout, and the retained
  values of a broker that is not up yet (`messengerd` is `init`'s child) kept,
  one per topic, and sent by the next publish or `flush`. `init` reads the
  login records and `pkgd`'s provisioning progress off the broker and checks
  the kernel-stamped `publisher` slot of every record against the manifest row
  it must be. `user::messenger::router`, its `*b"os.topic"` ids and fields and
  `svcpolicy`'s `Roster` and `Sink` are deleted.
* **xui: one subscriber.** `fabric::Follow` is a handle on one filter; the
  thread's hub owns one `Subscriber` under all its follows. A follow
  subscribes and attaches its doorbell when it is made; `Follow::take` (or the
  typed `fabric::pull`) looks at the doorbells, which are local queues, and
  drains a subscription only when its bell rang, so the UI thread never waits
  on the broker (the `UI:STALL` lines of `topic_feed::poll_waiting` cannot come
  back) and nothing polls it on a timer. A loop that wants to be woken parks on
  `Follow::bell` beside its other endpoints (`Resident::idle_with`); one with a
  tick of its own just takes. The hub is per thread, not a worker thread: a
  first attempt with one watcher thread per process failed on the device,
  because Messenger handles and an app's label (`subscribe:` permissions)
  belong to the thread that made them (`EACCES` on the worker's subscribe,
  `ENOENT` waiting on a handle another thread made). `fabric::Watch` is the runtime's
  `topics::Subscribe`, so `subscribe_<topic>(&mut Watch, ..)` gives a typed
  `fabric::Feed<Payload>`. Moved: `netstatus`, the Terminal's change feed, the
  tray generation (`TrayIcon` and the shell), the shell's resident-apps and
  failure feeds, and the LazyRAD handoff's install wait (a `PkgEvents` seam,
  scripted by the handoff tests). The first read of a retained value no longer
  waits: the shell follows its generation topic from its start, and the item
  `Set` of a `TrayIcon` goes ahead with the generation it knows (a newer one
  re-registers it).
* **What booting it found.** `messengerd` kept 64 retained values and 64
  subscriptions; with `init` and `healthd` on the broker the retained service
  and health rows alone filled the first, and a later retained value (the
  session's resident apps) was dropped silently: 1024 and 256 now. A labelled
  app could not attach a doorbell (`Bell` was not among the methods a
  `subscribe:` permission compiles to): `pkgstore::rules` allows it. The broker
  rings a bell after it answers the publisher, so a `Launch` sent right after
  a login record can beat the ring: `init` asks the broker itself (bounded,
  `Feed::drain_waiting`) before it refuses a `Launch` into a session whose
  owner it does not know yet. Not verified on a boot: the Terminal following a
  theme change (the image had no BusyBox, so the Terminal could not start a
  shell and `confctl` could not run).
* **Declared or named.** `demo/waitany`, `topics/depth` and
  `system/events/selftest/forbidden` are probe topics declared in
  `idl/topics.midl`. `system/events/security/denial` and
  `system/events/audit/overflow` are journal keys `logd` appends under (not
  topics anything publishes): `logstore::source` constants.

### 3.6 Generated crate layout

Split `libs/generated/src/lib.rs` into one module file per interface and gate
each behind features (`<iface>`, `<iface>-client`, `<iface>-server`). An app
then compiles and links only the interfaces it names, which matters for the
`-Os` program size goal, and a reviewer can read one interface's output.

### 3.7 The lint that keeps it true

`tools/midlc/check_handrolled.py --ratchet` (a CI step, like
`check_adhoc.py`): outside `libs/generated`, `libs/messenger`, the two
transports and the kernel's IPC core, it counts `Header {` literals,
`ERROR_FIELD` scans, `METHOD_* =>` arms, `"os.lazy.` service-name literals and
`fnv1a64` definitions, and fails when a file has more than its baseline. The
baseline only goes down.

As built: the lint is `tools/midlc/check_handrolled.py` with the baseline
`tools/midlc/handrolled_baseline.json`. `Header {` counts only a literal whose
first field is `version` (GPT, ELF and similar headers do not match); a
`METHOD_X | METHOD_Y =>` arm is one hit; `fn error_reply` and `fn parcel(..
method ..)` count as reply helpers. Not scanned: the generated crate and Rhai
bindings, `libs/messenger`, `libs/lazyos-sys`, the two transports
(`user/src/messenger/client.rs`, `xui-app/src/platform/messenger.rs`), the
whole of `kernel/`, `@generated` files and test code (`tests/`, `fixtures/`,
`benches/`, `tests.rs`, `*_tests.rs`, `test_*.rs`, `mock*.rs`, and a file's
`#[cfg(test)]` tail). `--update-baseline` only lowers counts and drops zeroed
files; `--init-baseline` rewrites it from the tree.

## 4. xui-app: one app shape

### 4.1 One entry point

```rust
fn main() -> ! {
    xui_app::app(Channel::Pdf, "PDF Viewer")
        .size(800, 600).min_size(420, 300)
        .opens(&["application/pdf"])       // argv, uri-list drops and "Open with" become Msg::Open
        .run(|ui, cx| PdfApp::build(ui, cx))
}
```

`app(..).run` does what the seven paths do today: connects (owner or client
mode), applies the theme once (through `system_theme`, nothing else),
installs the trace panic hook, emits `UP` on the first frame and `QUIT` or
`RUN:CLOSED` at exit through `trace`, and always exits through `finish`.
`.opens(..)` replaces the outbox, `argv::file_arg` and the start-up
`ui.emit(OpenChosen)`. `.resident()` / `.windowless()` cover the tray apps.
Delete `themed.rs`, the hand-written mains and `launch::run`'s variants;
`tools/xui/new_app.py` writes this shape (about 25 lines).

### 4.2 An app context instead of per-crate `Host`s

`cx: &AppContext` gives every app the same services, each built on a
generated client:

* `cx.settings::<T>()`: a typed per-app store over `confd` at
  `user/<uid>/<app id>/..` (serde), with a change subscription. Replaces the
  `format!` keys, LazySky's TOML and NES's `recent.txt`.
* `cx.dirs()`: home, documents, app data, transient, all from `libs/fhs`.
* `cx.recent()`: recent files, stored through `settings`.
* `cx.open_dialog(filter)` / `cx.save_dialog(..)`: the `FileDialog` preset
  (file system, start directory, "All files", cancel) with the recent list.
* `cx.clipboard()`, `cx.notify(..)`, `cx.error(title, &err)` (one error
  dialog that renders a `CallError` well), `cx.tray()`.
* `cx.files()`: the file-system seam the four `Host` structs carry, with an
  in-memory implementation for host tests.

The portable crates (`archiver`, `pdfview`, `videoview`, `writer`) take
`&AppContext` (or the trait it implements) instead of their own `Host`.

**As built (X2).** `.run(|ui, cx| ..)` hands the make closure an
`xui_app::AppContext` (`xui-app/src/context.rs`); `cx.backend()` is the old
second argument. The portable crates cannot depend on `xui-app`, so the seam
is a tiny crate, `xui-app/crates/host` (`xui-host`, depends on `xui-core`,
`xui-work`, `confd`, serde):

* `AppHost` (+ `Report`): `file_system()`, `start_dir()`, `temp_dir()`,
  `launch(path)`, `read_only_roots()`, `write(path, bytes)` (atomic on
  LazyOS), `suggests(dir)`, and `report(line)`. `cx.host()` is the LazyOS
  implementation; `testing::StdHost` (real files, records its evidence with
  the channel prefix, `on_launch`/`on_write` hooks) and `testing::MemHost`
  (an in-memory `FileSystem`) are the host tests'. `archiver`, `pdfview`,
  `videoview` and `writer` take a `SharedHost` (`Rc<dyn AppHost>`); their
  `Host` structs are gone. Writer keeps a small `Services` (font families,
  the print queue, the remembered printer) for what is not shared.
* `dialogs::{open_dialog, save_dialog, preset}` and `ErrorDialog`:
  `cx.open_dialog(ui, title, filters, on_cancel)` is the file system, the
  start folder, the filters, "All files" last and `require_existing`;
  `cx.save_dialog` the same without the forced "All files"; `cx.error(ui,
  title, on_close)` builds a message dialog whose `show` takes anything that
  displays, so a `CallError` renders as the service's sentence plus hint.
* `settings::Settings<T>` (`cx.settings::<T>()`): serde struct to one `confd`
  key per leaf under `user/<uid>/<settings id>/` (the channel prefix in lower
  case, or `.settings_id(..)`; `volume` keeps `audio`); read once at open (a
  missing or mistyped key is the field's default), saved by field diff on a
  worker thread, `on_saved(Sink<()>)` rings when a save landed. Floats and
  lists are stored as JSON text in a string key; maps are not supported. With
  `confd` not registered yet the settings live in memory (no start-up wait).
  `cx.settings_key(name)` / `user_key(uid, app, name)` build the key for a
  per-name setting that is not a struct field (term, volume, LazyWeb).
* `recent::Recent` (`cx.recent()`): at most 10 files, newest first, kept
  through settings; `cx.open_dialog` opens in the newest one's folder when the
  app asked for the list. `cx.dirs()`: home, start, documents, downloads,
  app data (`.system_name(..)`), transient, tmp.
* **Evidence.** A marker site with a host in reach calls `report!(host,
  "OPEN:PASS:{path}")`; the line is the text after the channel prefix and the
  host writes it as `trace::line!(channel, ..)`, so the bytes on the serial
  port are what `println!` wrote. `settings` and `confd-editor` report from
  free functions and worker answers, so they use `xui_host::evidence!`, the
  process's one sink (`AppContext::new` installs it). 26 prints left
  `tools/trace/adhoc_baseline.json` (writer 12, settings 9, confd-editor 4,
  pdfview 1); archiver, videoview and pdfview's `host.log` lines (dynamic
  prefixes the lint could not see) are reports too.
* NES keeps its `Host` trait (audio and the `Report` enum are its own) but
  loses `default_dir` and `recent.txt`: the picker comes from the context and
  the recent list from `cx.recent()` (the old file is not read; no backward
  compatibility). LazyWeb's `Host` is engines and probes, not files, so it
  stays; only its confd keys use `user_key`.

Not migrated: LazySky's `settings.toml` (the `tools/lazysky` harness writes
that file from the Terminal and greps `SKY:SETTINGS:SAVED:<path>`; moving it
needs the harness reworked first), and the remaining `FileDialog` chains in
paint, term, lazysky-ui and the Settings picture picker.

### 4.3 One way off the UI thread

`cx.spawn(work, Msg::Done)` runs `work` on a worker and delivers its result
through the backend's waker (`backend/wakeup.rs`) as a message. It replaces
the timer-polled mpsc and mutex slots in pdfview, archiver, golf, LazySky
and `net/web.rs`, and is how a `Pending<R>` from a generated async call
reaches the UI. Progress uses the same channel.

**As built (X3).** The helper is the tiny crate `xui-app/crates/work`
(`xui-work`, re-exported as `xui_app::work`), over xui's `Proxy` (whose
`send` already ends in the backend's waker). It lives in its own crate rather
than in `xui-app` because the portable, host-tested app crates (pdfview,
archiver, golf, lazysky-ui, explorer, settings) depend on `xui-core` only and
must not gain `xui-app`; `xui-work` needs nothing else. There is no
`AppContext` yet (X2), so the calls take the window's `ui.proxy()` (or a
`Sink`) instead of `cx`:

* `work::spawn(&proxy, "name", || work(), |result| Msg::Done(result))`: a
  named thread; `result` is `Result<R, Failed>`, so a thread that cannot
  start or a panic still answers (under the image's `panic = "abort"` a panic
  ends the process, as everywhere).
* `work::spawn_with(&proxy, name, |report| .., Msg::Progress, Msg::Done)`: the
  same with `report.report(p)`, `report.paced(gap, || p)` and
  `report.cancelled()`; the returned `Job` cancels on drop (`detach()` opts
  out).
* `work::Queue`: one long-lived worker for a stream of requests (`start`, or
  `start_latest` to skip a burst's stale ones): the docs loader and the
  desktop scanner.
* `work::Sink<R>` (a handle that posts `R` as the app's message; `Sink::bell`
  for a doorbell) and `work::inbox` (a queue of small results behind one
  doorbell message, drained in batches): pdfview's render threads, LazySky's
  layout thread, the explorer's thumbnails and paste, LazyWeb's relay.

Migrated: pdfview (tiles as `Msg::Tile`, a document epoch drops tiles of a
closed file; the 30 ms drain timer is gone), archiver (`Msg::Progress` and
`Msg::Finished`; `lazyarc::Progress::observed` tells the window at most every
100 ms; the poll timer is gone), golf (`Msg::Gen`, a generation number drops
a superseded search), lazysky-ui (layout rows through an inbox; a `Msg::Laid`
doorbell promotes the tick), explorer (thumbnails through an inbox, paste
through `spawn`), settings (elevated changes through `spawn`, `Done::Lost` on
failure), docs loader and `desk-scan` (`Queue`), Net Tools' fetch (`spawn`,
the status refresh timer stays), LazyWeb (`Waker` replaced by `Sink::bell`).
Not migrated, on purpose: frame clocks (golf's and the video player's frame
timers pace drawing, they do not poll for a result), LazySky's tick for the
core's events and pictures (its own design, R5/R8), the Playground's runner
wake, and LazyWeb's helper threads whose doorbell already is a `Proxy`
send.

### 4.4 Shared plumbing, once

* One `atomic_write` (in `libs/fhs` or a small std helper crate) for the 16
  copies.
* One image decode path (the `image` crate with the formats enabled once)
  for explorer, LazySky, pdf and video.
* Paint uses `crates/archive` for zip and deflate; its own copies go.
* `crates/https` owns the CA bundle lookup and serves Mail and the plain
  HTTP code in `src/net`.
* Lists default to `SortedTable`; a raw `ListView` stays for plain lists.
* Every evidence line through `trace`; the `println!` markers move and the
  `check_adhoc.py` baseline drops with them.

**As built (X4).**

* `xui-app/crates/atomicfile` (std only, no dependencies): `write` and
  `write_with` create the temp file exclusively beside the target, sync it,
  give it the target's permissions, rename it over the target and sync the
  directory; a symlink target is refused. Paint's and Writer's storage, the code
  editor, LazySky (settings and saved pictures), printd and LazyWeb (session,
  visits, places, notifications, blocker lists) use it. Left on purpose: the
  Archiver's extract and create (they stream to a path the format writers open
  themselves, and extract replaces a link rather than following it), downloads'
  `.part` rename (a finished download, not a rewrite) and the Explorer's moves.
* Paint's inflate and CRC-32 tables are gone (`flate2` and `crc32fast`, the
  copies `lazyarc` links). Its small in-memory zip container stays: `lazyarc`'s
  zip reads and writes `std::fs::File`, and Paint's crate does no I/O.
* Image decoding needed no change: the PDF crate's `png` is a dev-dependency of
  an example, `video` and the `image` crate already share one `zune-jpeg`, and
  Explorer and LazySky enable different `image` features, so merging them would
  link codecs an app does not use.
* `lazyhttps::Roots::lazyos()` is the one CA bundle lookup (LazySky's
  `ca_roots`); Mail installs its TLS provider with `lazyhttps::install_tls_provider`.
  Net Tools' `net::{http, web}` stay a raw HTTP/1.0 socket probe (resolved
  address, bytes counted with headers, timing), which is what the app shows.
* `xui_app::platform::dirs::home()` is the one `$HOME` lookup (no filesystem
  access, so the UI thread may call it); `default_dir()` adds the existence
  check and the `/transient` fallback. Paint, NES (now `fhs::app_data_dir`),
  the Terminal, LazyWeb's profile and the desktop folder use them.
* 289 `println!` markers moved to `trace::pass!`/`fail!`/`line!` with the same
  text on the wire; the baseline fell from 319 to 30 prints in `xui-app` and `lazyrad-os`. Left: Settings, Writer,
  confd-editor and pdfview (portable crates `xui-app` depends on, no seam yet),
  `shell/failures.rs`, and LazyWeb's host-build fallback print.

### 4.5 Async: one non-blocking form, no `async fn` API (yet)

Async/await works in LazyOS Rust, but only `user::messenger_async` uses it
(its own `Call`/`Recv` futures, a round-robin `Executor`, `block_on`, the
`Selector` multiplexer and the `service!` macro), and only the
`async_echo`/`async_service` demos use that. The kernel, `libs/`, `xui-app`
and `lazyrad-os` have no `async fn`; Mail's tokio comes from esMail's
`async-imap` and stays inside it.

Async would not shrink the boilerplate this plan targets: that is framing
(headers, error scans, dispatch, names, connect logic), which generation
removes, and a generated blocking call is already one line
(`confd.get(path)?`; `.await` makes it no shorter). Most app worker threads
(49 files spawn one) do CPU work or call blocking libraries (`ureq`), where
async gains nothing and would need an async network stack LazyOS lacks. xui
is message-driven (`update(Msg)`), so async there means an executor in the
backend loop and a second programming model, the opposite of section 2.3.
It would cost a second generated form of every method, coloured functions,
`Pin`/`Send` rules and binary size against the `-Os` goal.

So each generated method gets two forms:

* `iface.method(args)`: blocking, bounded by the IDL `deadline`.
* `iface.method_begin(args) -> Pending<R>`: non-blocking. In a native
  program `Pending<R>` is a `messenger_async::Call` future, usable with
  `.await` or a `Selector`; in an xui app it completes as a `Msg` through the
  backend waker (`cx.spawn`, section 4.3).

`service!` and the hand-written mailbox health parcel in `messenger_async`
are replaced by the generated server trait and `dispatch` (C4). Async/await
is reconsidered only if, after C5, services that make several dependent calls
while serving turn out to be common; by then the remaining boilerplate is
easy to measure.

## 5. Stages

Each stage lands as its own PR, deletes what it replaces in the same PR, and
leaves `main` green.

| Stage | Content | Removes |
|---|---|---|
| C1 | `Transport`, `Connection`, `CallError`, `Errno`, `Mock` in `libmessenger`; native and std transports; registry-subscription wait | the four connect/cache implementations, `CONNECT_TICKS` copies |
| C1b (built) | Kernel registry: `WaitFor` parks a resolve until the name registers, with kernel correctness and stress tests (`ipc_registry_wait_*`); both transports block on it | the resolve sleep loops |
| C2a | midlc: `service`, `errors`, `scope`, `deadline` syntax and their generated constants; per-interface modules and features | service-name literals, `fnv1a64` copies, confd/clipboard constant copies |
| C2b | **Built.** Rust client generation over C1's `Transport` (blocking forms; `_begin` waits for an asynchronous transport call); host tests through `Mock` (`libs/generated/tests/client.rs`); `timed` and `notify` migrated as proof | (enables C3) |
| C3 | Migrate clients interface by interface (confd, init, topics, clipboard first: the most duplicated); `user::messenger::*` and `xui_app::platform::*` keep only domain adapters. **Native half built for** confd, mimed, pkgd, accountsd, keyd, logind, mountd, devd, gpud, usb audio, init, healthd, logd, sysmond and the clipboard (`user/src/messenger/<svc>.rs` is `pub type Client = wire::Client<Native>` plus domain meaning; slow calls keep `Deadline::Never`, `Nested` carries the clipboard's `ALLOW_NESTED`). **Also built:** `net` (NIC), `netstack`, `netsock`/`netstd`, `wifi`, `display`, `input`, `logind`'s console session, and the `audioclient`/`trayclient` seams (see the "as built (C3, the rest)" note). Left: `webd`, `audio_events` (a topic publisher, C5) and the hostile-input probes | per-module `fn header`/`fn parcel`/`error_code`, `Service::call_*` |
| C5 | **Built.** Typed bell subscriptions (generated `subscribe_<topic>` returns a decoding stream; `central::Feed` natively, `fabric::Follow` and its per-thread hub in xui); `init`, `healthd` and their readers moved off the interim router; the undeclared topics declared or named | `topic_feed`, `router` (422 + 132 lines), `Roster`/`Sink` (588), the shell failures poller, the handoff poller |
| C4 | **Generator and 27 servers built** (section 3.3 lists what is migrated and what is not): `Server` and `dispatch` per interface over `libmessenger::server`, every method required; `xuid`, `audiod` and `webd` still hand-write theirs | dispatch arms 469 to 347 (the rest are event decoders and the unmigrated servers), `error_reply` helpers, `xui-app`'s `reply_parcel`/`error_parcel` |
| C6 | **Built.** `tools/midlc/check_handrolled.py --ratchet` in CI (the `trace` job; baseline `handrolled_baseline.json`, self-test `test_check_handrolled.py`); baseline 355 hits in 86 files | (keeps it from coming back) |
| X1 | `xui_app::app(..)` builder, `.opens`, panic hook, `finish`; new template; migrate every bin | `themed.rs`, outboxes, seven launch paths |
| X2 | **Built** (section 4.2, "As built"); LazySky's TOML and four more dialog chains wait | four `Host` structs, `format!` confd keys, `$HOME` lookups |
| X3 | **Built** (`xui_app::work`, section 4.3; `cx.spawn` follows X2) | four worker handoff mechanisms |
| X4 | **Built** (section 4.4, "As built"); the portable crates' markers moved with X2's evidence seam | atomic-write, image, zip, HTTP copies; `println!` markers |

C1, C2a and X1 are independent and run in parallel; C2b needs C1 and C2a;
C3 and C4 need C2b; C5 needs C1; X3 follows X1 (they touch the same bins);
X2 follows X1 and uses generated clients once C3 has them.

## 6. How we will know

* Lines: `user/src/messenger` (10,520 today) and `xui-app/src/platform`
  (4,839) shrink to their domain adapters; the hand-roll lint baseline and
  the `check_adhoc.py` baseline fall to zero for migrated files; a typical
  small bin is under 40 lines.
* Behaviour: the same call has the same deadline, retry and error text in
  every client (one conformance test per generated client against the
  corpus in `idl/conformance`, run on the host through `Mock`).
* Performance: no timer-driven poll left in the client runtimes; per-app
  binary size does not grow (features keep unused interfaces out), measured
  on the image before and after C3 and X1.
* Kernel: the registry wait (C1b) is the only kernel change; if the
  registry is kernel-served it gets correctness and stress tests in its suite
  (many waiters, names that never appear, register/unregister churn).

### Simplification pass (as built)

A review pass over the new layers removed what had no caller and left the
structure alone. Hand-written production Rust (`measure.py`, tests and
generated code excluded) went from 542,381 to 541,240 lines (-1,141). Of
that, 633 lines are two test doubles that lived in `src/` and are now named
like the tests they serve (`xui-app/src/fabric/subscribe/test_fake.rs`,
`libs/messenger/src/client/test_mock.rs`); the other -508 are deleted code
(all Rust including tests: -532).

* One connect bound: `Transport::connect_bound` defaults to
  `CONNECT_BOUND` (10 s) in `libmessenger`; the native, std, `Nested`,
  `Held` and `Observed` transports no longer repeat it.
* Deleted, with no caller anywhere (found by counting every identifier in
  the tree, docs and scripts included): `Settings::on_saved` and its
  `Arc<Mutex<Option<Sink>>>`, `ErrorDialog::show_while/show_titled`,
  `Queue::start_latest`, `atomicfile::write_with`, `AppContext::settings_key`,
  `xui_host::testing::MemHost/MemFs`, `StdHost::on_launch/shared_lines`,
  `user::messenger::services::drivers`, `resolve_service`,
  `TcpStream::wait_readable`, `global_stats/global_totals`, timed
  `TICK_TOPIC/decode_tick`, and about twenty unused accessors in fatvol,
  netstack, nes, paint, playground, golf, webd and writer.
* Looked at and kept: the `Follow`/`Feed` pair (std hub over `Subscriber`
  versus the native `central::Feed`) share a contract, not code: one is a
  thread-local hub over a nested-call transport, the other owns a `Bus` and
  a bump-allocator buffer, and merging them would put `std` types in
  `user`. `Connection::call_once_more`/`with_retry` differ in what a retry
  is allowed to resend. The broker client (`user::central`, `topics_client`)
  is still hand-framed; moving it to the generated `topics` client and the
  generated server in `messengerd` is the one change large enough to reach
  the -1,500 line mark (about 750 lines of `topics_client` plus `Bus`), and
  it needs `messengerd`'s soak run and a boot, so it is a stage of its own.
