# Client API plan: generated Messenger clients and one xui app shape

A review of the API every LazyOS client program uses (the Messenger runtime,
the code `midlc` generates, and the `xui-app` layer desktop apps build on),
with a plan to make apps smaller, make every client behave the same way, and
make the IDL the only place a protocol is written.

Status: proposal (2026-11-11). Nothing here is built yet.

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
which polls `NextEvent` with an expired deadline; and `shell/failures.rs`).
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
  callbacks). `archiver/src/job.rs:4` and `pdfview/src/worker.rs:5` say the
  backend has no cross-thread waker; `backend/wakeup.rs` is one.
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
eviction on `EPIPE`/`ENOENT` and one reconnect. **Built in C1** (module
`libmessenger::client`; transports `user::messenger::client::Native` and
`lazyos_sys::client::Std`; `Mock` behind the `mock` feature). C1 left the
registry wait below undone: each transport still polls inside `resolve` for
its one runtime bound (10 s; 10 ms sleeps in std, one tick natively), the
only polling loop per runtime, until the kernel registry can wake a waiter. Waiting for a service that has
not registered yet blocks on the registry, not on `sleep_millis(10)`: the
registry (`idl/registry.midl`) has no way to wait for a name today, so C1
adds one (a retained `system/registry/registered/{name}` topic, or a
`WaitFor(name)` method answered when the name appears), and the "never poll
on a timer" rule then holds here too. `Errno` is a named newtype
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

### 4.3 One way off the UI thread

`cx.spawn(work, Msg::Done)` runs `work` on a worker and delivers its result
through the backend's waker (`backend/wakeup.rs`) as a message. It replaces
the timer-polled mpsc and mutex slots in pdfview, archiver, golf, LazySky
and `net/web.rs`, and is how a `Pending<R>` from a generated async call
reaches the UI. Progress uses the same channel.

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
| C2a | midlc: `service`, `errors`, `scope`, `deadline` syntax and their generated constants; per-interface modules and features | service-name literals, `fnv1a64` copies, confd/clipboard constant copies |
| C2b | Rust client generation over C1's `Transport` (blocking and `_begin` forms); conformance tests for the client against the corpus through `Mock` | (enables C3) |
| C3 | Migrate clients interface by interface (confd, init, topics, clipboard first: the most duplicated); `user::messenger::*` and `xui_app::platform::*` keep only domain adapters | per-module `fn header`/`fn parcel`/`error_code`, `Service::call_*` |
| C4 | Server generation and `dispatch`; migrate daemons, starting with small ones (`timed`, `mimed`, `pkgd`) then `xuid`, `netd`, `inputd` | 469 dispatch arms, 16 `error_reply`, `xui-app/src/server.rs` |
| C5 | Typed bell subscriptions; move `init`/`healthd` off the interim router; declare or drop the undeclared topics | `topic_feed`, `router`, three ad hoc pollers |
| C6 | `check_handrolled.py --ratchet` in CI | (keeps it from coming back) |
| X1 | `xui_app::app(..)` builder, `.opens`, panic hook, `finish`; new template; migrate every bin | `themed.rs`, outboxes, seven launch paths |
| X2 | `AppContext`: settings, dirs, recent, dialogs, error, files | four `Host` structs, `format!` confd keys, `$HOME` lookups |
| X3 | `cx.spawn` | four worker handoff mechanisms |
| X4 | Shared plumbing (4.4) and the marker migration | atomic-write, image, zip, HTTP copies; `println!` markers |

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
* Kernel: the registry wait in C1 is the only change near the kernel; if the
  registry is kernel-served it gets correctness and stress tests in its suite
  (many waiters, names that never appear, register/unregister churn).
