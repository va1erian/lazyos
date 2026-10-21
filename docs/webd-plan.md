# webd: one daemon that owns WebKit, and a small API for everyone else

Status: **plan**, P0 spike (a), P1.0 (webkit-app removal) and the engine half of P1 (Host/View, section 7) done 2026-10-21; the engine and the daemon are not built. Builds on W4/W5/W6 of
[`webkit-port-plan.md`](webkit-port-plan.md), [`lazywebplus.md`](lazywebplus.md) and
[`../webkit/README.md`](../webkit/README.md). Figures marked *est.* are estimates, not measurements.

## 0. The proposal in one page

`webd` is a per-session resident package (`os.lazy.webd`) that is the **only** program linking WebKit's UI
process. It owns the web engine's processes, the profiles (cookies, cache, storage), the ad-block lists, downloads,
notifications and the navigation policy. Any app asks it for a **view** over Messenger and gets back a stream of
frames in shared memory plus page events; the app forwards input. A client is a few hundred lines and links no
WebKit: LazyWeb+ shrinks to chrome, Mail can show an HTML message with scripts and network off, a LazyRAD form gets
a `WebView` control, a package can be "just a URL".

Recommendations, each argued below:

| # | Decision | Why |
|---|---|---|
| D1 | webd is a **core package** (label `app:os.lazy.webd`), resident and autostarted per session (the LazyCal model) | engine children are confined only through a package's `[processes.*]` tables (`process-sandbox.md`); it holds the user's cookies so it must run as the user, never as a privileged service like `printd` |
| D2 | Pixels move as **client-owned shared-buffer slots with damage rectangles**, the display protocol's `Present`/`BufferRelease` shape | it reuses `xui-webkit`'s delta, scroll-ahead and band logic unchanged on the client; a compositor subsurface can replace it later without touching the API |
| D3 | The C++ engine becomes **multi-view first** (`Engine E` global to `View` objects), with the two existing browsers running on a one-view adapter until they move | it is the bulk of the risk, it is useful alone (tabs), and it can land before any daemon code |
| D4 | **Profile = caller's label** by default; a shared `browser` profile needs a manifest scope; web processes are never shared across profiles; one NetworkProcess for all, with per-profile cookie jars at first | memory is the scarce resource (107 MiB per process, no text sharing); isolation of cookies and storage is the property worth paying for |
| D5 | Navigation/handoff/download policy is **declared per view** and enforced inside webd; questions that need the app come back as asynchronous events with a deny-on-timeout | `lzw_ui_navigate` is a synchronous call today and cannot cross a process boundary |
| D6 | **One web view, not two: remove `webkit-app` (W5) first, before the multi-view refactor**, then remove **Blitz** (LazyWeb, Docs' and Mail's `BlitzView`) once webd carries their views (section 9) | LazyWeb+ is a superset of W5, so the refactor has one consumer of the C ABI instead of two; after Blitz is gone there is one web renderer and one font path |
| D7 | **Light views**: a second, smaller interface (`os.lazy.webd.light.v1`) for callers that need HTML/CSS rendering only: no network at all, scripts off by default, no storage, none of a browser's client machinery (3.6) | most callers (Mail, Docs, help, forms) are not browsers; separate interfaces make "cannot reach the internet" a kernel-enforced property and make Blitz removable before the browser moves |

## 1. Why now: what the code says

* **Every browser carries its own engine host.** `lazywebplus-app` and `webkit-app` are each a 108 MiB static
  program (`webkit/README.md`); LazyOS loads a static ELF whole into RAM with no text sharing
  (`webkit-port-plan.md` section 6, "Memory for code"), so every UI process, WebProcess and NetworkProcess costs
  about 107 MiB before the first page. Today: one browser = 3 x 107 MiB, a second browser = 3 more.
* **The engine host is single-page.** `Engine E` in `webkit/port/app/engine.h` is a process global holding one
  context, one data store, one `WKView`, one `WKPageRef`; `lzw_ui_*` / `lzw_engine_*` in `lazyos_ui.h` carry no
  view id. No tabs, no second window (`lazywebplus.md` "Known limits").
* **The seam already exists.** `xui-webkit`'s `Engine` trait is "what a window asks of the engine", the `Mailbox`
  is "what the engine tells the window", and the whole window is host-tested against a recording engine.
  A remote engine is a third implementation of the same trait.
* **Heavier consumers use a weaker engine.** Mail's reading pane, Docs and LazyWeb are Blitz views (no script,
  partial CSS). LazySky-style sign-in flows and HTML mail have no engine to use.
* **Shared state is currently per-app by accident.** The ad-block store compiles EasyList in the UI process
  (391 MiB peak in the host measurement, 4.3 s in the guest) once per browser; the cookie jar and HTTP cache
  live under `$HOME/.apps/<browser>/`.
* **The engine host is the least stable program we ship**: patches `0021` and `0027` leak proxies to avoid
  aborts, #853 is a startup crash in about every second launch of one harness, #865 is a freeze cascade.
  Putting every client's pages behind one UI process raises the cost of each of those (section 8).

## 2. Goals and non-goals

Goals: one owner of WebKit; a view API small enough for a Rhai script; shared network process, cache, blocker,
fonts and one UI process; per-app isolation of cookies and storage; clients that link no WebKit (a licence and a
size firewall); a path for LazyWeb+ to become chrome only.

Non-goals (first cut): GPU compositing; a compositor subsurface (phase P6); webd-owned top-level windows and
web apps (P4); multi-user simultaneous sessions beyond what the display supports; WebGL, media, WebAssembly
(engine limits, unchanged); replacing Blitz where it suffices (Docs, Markdown help stay on Blitz).

## 3. Architecture

```
 LazyWeb+ (chrome)   Mail (HTML pane)   LazyRAD WebView    any xui app
        \                  |                  |                /
         `----- os.lazy.webd.view.v1 (Messenger: calls, oneway events, Buffer slots) ----'
                                   |
   +-------------------- webd  (app:os.lazy.webd, one per session) -------------------+
   |  service thread (Rust)         view table, quotas, profile map, policy, slots     |
   |      |  lzw_engine_run_on_main / mailbox                                           |
   |  WebKit main thread (C++)      one WKContext, N WKPage/WKView, data stores,        |
   |                                paint scheduler, children.cpp, notify, blocking     |
   +--------+-------------------------------+-----------------------------------------+
            | fork+exec (confined labels)   |
   app:os.lazy.webd/webprocess  (per profile, pooled)     app:os.lazy.webd/networkprocess  (shared)
                                                          app:os.lazy.webd/listupdate      (curl)
```

### 3.1 Deployment (D1)

* Package `xui-app/packages/webd`, `[entry] resident = true`, `abi = "linux"`, `autostart = true`: `init`
  starts it with `--background` at login (section 8 for start, stop and the not-running case). Resident apps always get a tray item; webd's is useful (views per app, memory, "end all"),
  or the `resident` rule gets a `hidden` flag (small `lazypkg` change).
* The three children keep the sandbox of `lazywebplus/manifest.toml` (`[processes.webprocess]` reads fonts only;
  `networkprocess` has sockets, resolver, CA bundle and the profile folders). Profile root:
  `$HOME/.apps/os.lazy.webd/profiles/<profile>/`. The `Downloads` write grant moves here.
* **Naming (spike P0a, resolved).** A labelled app may register only `app.<id>.<name>` and advertise only
  interfaces of its own domain `<id>.<name>.v<N>`; the kernel does this and has tests for it
  (`label_suite/names.rs`: `app_namespace`, `cross_app_resolve`, `own_domain_interfaces`,
  `foreign_interfaces_denied`). The interface is `os.lazy.webd.view.v1`, served as `app.os.lazy.webd.view`.
  What was missing is only on the client side: a manifest entry grants resolving the interface name, the name
  without `.v1` and the `SERVICE_NAMES` extras, none of which is the registered name. The fix is one row
  (`("os.lazy.webd.view.v1", "app.os.lazy.webd.view")` and the same for `os.lazy.webd.light.v1` in
  `libs/pkgstore/src/rules.rs`, done in this branch with a host test, `cargo test -p pkgstore` passes), the same row the LazyCal branch
  (`claude/lazycal-autonomy-impl-e2aa0a`) adds for `os.lazy.lazycal.invites.v1`; when both land the generic rule
  that plan describes (an interface `<id>.<name>.vN` of an app's domain is served as `app.<id>.<name>`) should
  replace the two rows. The consent row in `pkgstore::explain` waits for `idl/webd.midl` (its table test
  requires one row per declared interface). **No kernel change is needed**, so the fallback of a core-package
  flag is dropped.
* One session at a time: every call checks that the kernel-stamped session equals webd's (the tray's rule,
  `tray-plan.md`); a second session's webd would need its own name, which the display does not support yet.

### 3.2 Inside webd

* **Rust half** (`xui-app/crates/webd`, portable and host-tested against an in-memory fabric and the recording
  engine, like `xui-webplus`): the view table, quotas, profile map, policy evaluation, slot state machine, the
  Messenger service. It runs on **its own thread**: a Messenger handle belongs to the thread that made it, and the
  notification service accepts only the process's first thread (`lazywebplus.md`, "Toasts are made on WebKit's
  main thread"). Everything that touches WebKit goes through `lzw_engine_run_on_main`; everything WebKit says
  arrives as `lzw_ui_*` calls that only queue. This is the arrangement the window already has, so it is proven.
* **C++ half** (`webkit/port/app/`, ~2,800 lines across `engine`, `clients`, `policy`, `uiclient`, `paint`,
  `notify`, `blocking`, `children`): `Engine E` splits into `Host` (context, data stores, children list, run
  loop) and `View` (page, view, size, dialogs, downloads, hover, popup state, paint shadow). Every callback finds
  its `View` from `clientInfo`. The ABI (`lazyos_ui.h`) gains a `view` argument on every `lzw_ui_*` and
  `lzw_engine_*` that is about a page and `lzw_engine_view_create/destroy`; global ones (children, quit, shutdown,
  run_on_main) stay. The Rust half is a static library again, so `main.cpp` stays the entry point.
* **Painting.** `WKPagePaint` already takes a destination pointer (`paint.cpp`), so webd paints damaged
  rectangles **straight into the client's slot** and drops `E.pixels` and the mailbox copy. A paint scheduler
  runs on the main thread: round-robin over visible views with a per-pass budget, background views (`SetVisible
  false`) get no paints and a lower timer rate. This is the fairness the single-page UI process never needed.

### 3.3 Profiles, processes, isolation (D4)

* A profile is a `WebsiteDataStore`. Default key = the caller's label, so `app:os.lazy.mail` and
  `app:os.lazy.lazywebplus` never see each other's cookies. A caller may name `profile = "browser"`; that needs the
  scope `os.lazy.webd.browser.v1` (consent: "Browse the web and keep your cookies and history"), granted to
  LazyWeb+ and later browsers, so two browsers share a session. `profile = ""` + `ephemeral` is an in-memory store
  (`WKWebsiteDataStoreCreateNonPersistentDataStore`), the Mail case.
* WebKit keys web processes by data store, so views of different profiles never share a WebProcess. Within a
  profile, `engine.cpp` today turns process swapping, pre-warming and the process cache off (quota, patch notes in
  `webkit/README.md`); webd keeps that and caps web processes per user (4 *est.*). The quota that forced the cap
  now charges reservations by resident page (`limits.md`), so re-enabling swapping per profile is a later
  experiment, not part of this plan.
* **One NetworkProcess** for all profiles, with a cookie jar and cache directory per profile. Its label can write the
  whole profile root, so a compromised network process can read every profile's cookies. That is the honest cost of
  not paying 107 MiB per profile; it is documented, and a second network process for the `browser` profile is the
  first knob to turn once file-backed `mmap` gives shared text (the kernel work the port plan already calls the
  item that matters most).
* The ad blocker moves in whole (`webkit/port/app/blocking.cpp`, `xui-app/crates/adblock`, `blocker/`): one
  compiled list store shared by every view, switches per profile in `confd` under `user/<uid>/webd/blocker/`.
  Only views with `blocking = true` attach the lists.

### 3.4 Frames, input, latency (D2)

* **Slots.** The client allocates 2-3 shared buffers of the view's size (BGRA or RGBA, a `format` field) and
  attaches them (`AttachSlot`); buffers travel only in requests, never replies (`messenger.md` section 3), which
  is why the client owns them. webd paints into a slot the client has released, sends `Frame(view, slot, seq,
  damage[], scroll{area, dx, dy}, scroll_offset, at_bottom)`; the client lays the rectangles over its persistent
  picture, then `ReleaseSlot`. This is `lzw_ui_frame_delta` with a pointer replaced by a slot, so
  `damage.rs`, `speculate.rs`, `bands.rs`, `glide.rs` and `smooth.rs` stay in the client crate untouched.
  Overscan bands (`lzw_ui_band`) travel in a band slot. A refused delta (`lzw_ui_frame_delta` returning 1) becomes
  "client asks for a full frame" (`Refresh`).
* **Input.** Mouse, wheel, key, focus are oneway calls. The client keeps merging pointer moves (`pointer.rs`,
  issue #890), now in its own process, so webd's main thread never sees a flood. webd remembers the time of the last
  real input per view for its second gesture lock (`xui_webkit::gesture`): the lock exists to stop a script
  from starting a program, and it must live where the navigation is decided.
* **Latency.** The path gains one Messenger hop each way (service thread to WebKit's main thread already exists).
  A hop is a message, not a copy (frames are pointers into shared pages); the cost is to be measured in P0
  before anything else is built. Success criterion: scroll and typing latency in the Ars Technica repro of
  `lazywebplus-performance-plan.md` within 10% of today.
* **Later (P6).** A `Subsurface` request lets the compositor read webd's slots directly and route input to webd,
  removing the client blit and the forwarding hop. Not needed for correctness; the API does not change.

### 3.5 Policy, dialogs, downloads, notifications (D5)

`ViewConfig` (sketch in section 4) fixes behaviour at creation, so the synchronous callbacks of the UI process need
no round trip:

| Concern | Today | In webd |
|---|---|---|
| Which schemes load | `policy::decide` in the window | `scheme_policy` in `ViewConfig`: `web` (http, https, data, blob, about), `data_only`, or `origins[]` allowlist |
| `mailto:`, other schemes | gesture + 3 s real-input lock, then `mimed` | `handoff = deny | ask | browser`; `ask` emits `NavigationRequested(id, url, gesture)` and holds the listener (as dialogs do); no answer in 10 s denies. `browser` (needs the browser scope) hands to `mimed` from webd |
| `x-lazyweb:` internal pages | window-only | stays client-side: the client serves its own `data:` pages with `LoadHtml`, and `internal_origin` lets exactly that page issue client commands |
| `window.open`, `target=_blank` | `lzw_ui_new_window` | `NewWindowRequested(url, gesture)`; the client opens a view or ignores it; nothing opens on its own |
| Dialogs | `lzw_ui_dialog` + reply | same, per view; add a rate limit (a known gap) |
| Downloads | UI picks `.part` path in `$HOME/Downloads` | webd picks it (name sanitiser moves from `downloads.rs`), per-profile cap of 256, events `Download*`; `downloads = off | ask | allow` per view |
| Notifications | provider + bar + `notify.tsv` in the browser | provider in webd; permission store per profile; the *ask* bar stays in the client (`NotifyPermissionRequest`), toasts are posted by webd under webd's name unless the view says `notify_as = client` (open question) |
| Blocking | `lzw_ui_blocking(url)` | evaluated in webd from per-profile exceptions; the client toggles through `SetBlocking` / `ReloadUnblocked` |

### 3.6 Light views: no network, optionally no script, none of a browser's machinery (D7)

Most callers do not browse. Mail shows a message, Docs a rendered file, a settings page a help text, a
LazyRAD form a formatted label. They want WebKit's HTML/CSS/layout/font engine and nothing a browser needs. So
webd has two classes of view, and the second is a strict subset of the first:

| | **Light view** (`os.lazy.webd.light.v1`) | **Full view** (`os.lazy.webd.view.v1`) |
|---|---|---|
| Content comes from | the caller only: `LoadHtml(bytes, base_url)` | `Load(url)` over `http`, `https`, plus `LoadHtml` |
| Network | **none**: no scheme but `data:`, `blob:`, `about:blank`; every other load is refused in webd before WebKit is asked | per `ViewConfig` (`origins`, scopes) |
| Script | **off by default** (`scripts = false`); an explicit `scripts = true` is allowed (still no network, no storage) | per `ViewConfig` |
| Storage, cookies, cache | none: a non-persistent data store, no profile directory, no `$HOME` access | per profile |
| Navigation | locked: a link click, form submit or `meta refresh` becomes `LinkClicked(href)`; nothing loads | back/forward/reload, policy, hand-off |
| Browser features not installed | UI client (hover, cursor shape, `window.open`), JavaScript dialogs, downloads, notifications and their permission store, content blocking, navigation policy and `mimed` hand-off, responsiveness bar, history, the address and state events beyond title/load-finished | all of it, as in LazyWeb+ |
| Events the client gets | `Frame`, `Band`, `State(title, loaded)`, `LinkClicked`, `ClipboardWrite(text)` (Copy), `ViewLost` | the full list of section 4 |
| Calls the client may make | `CreateView`, `LoadHtml`, `Resize`, `SetVisible`, input, `Find`, `Zoom`, `Scroll`, `Destroy` | those plus navigation, dialogs, downloads, notifications |
| Permission a package asks | `os.lazy.webd.light.v1` only (LOW: "Show formatted text and pictures that you give it", nothing about the internet) | `os.lazy.webd.view.v1`, plus `os.lazy.webd.net.v1` and `os.lazy.webd.browser.v1` scopes when it needs them |

**Two interfaces, not one with a flag.** The kernel's rules are per interface and method (`rules.rs` allows an
interface whole), so splitting is how "this app cannot ever reach the internet through webd" becomes
enforcement, not a convention webd must remember. The light interface is a subset of the full one's methods
with the same ids and wire shapes (one `.midl` declares the shared structs, so the generated client code is
shared), served on its own name (`app.os.lazy.webd.light`; a `SERVICE_NAMES` row, in this branch). A package that lists only
the light interface cannot even resolve the full service.

**What it saves, by construction:**

* *Attack surface.* No network process involvement, no cookies, no storage directories to read, no hand-off path
  to `mimed`, no `notify`/download provider. A hostile message can still target HTML, CSS, image and font
  decoders and the JIT (when scripts are on), which is why scripts default off and why a light web process is
  still per client app (open question 9) and confined like `webprocess` (fonts only; a `[processes.lightprocess]`
  table that is the same binary under its own label, so a P5 attack harness can tell the two apart).
* *Code and work in webd.* The per-view clients that dominate `clients.cpp`, `uiclient.cpp`, `policy.cpp`,
  `notify.cpp`, `blocking.cpp` are not installed for a light view (`lzw_ui_features()` becomes a per-view
  mask, the mechanism already exists host-wide); it has no paint-pacing exceptions for animations, no hover
  traffic and no per-navigation callbacks. Main-thread cost per light view is a paint and an input path.
* *Memory.* No profile data store, no cache, no network-process entries; and **if a light context can avoid
  launching the NetworkProcess at all, 107 MiB less than a full view** (spike L1 below).
* *Client simplicity.* The light widget is `LightWebView`: `new(html)`, `set_html`, events. No chrome, no address,
  no downloads UI, no dialogs to draw.

**Spike L1 (P1): can a light view run with no NetworkProcess? No** (`webkit-views light`: one web process and one NetworkProcess for a non-persistent store and a `data:` page, launched as the page loads; `webkit/README.md`, "The engine"). The steps below are what was planned; (3) is the outcome, so the 107 MiB saving is dropped from the claims and a light view is cheap only in what it does not install.

Planned: Try in
order: (1) a second `WKContext` with `WKWebsiteDataStoreCreateNonPersistentDataStore` and only
`data:`/`LoadHtml` content; see whether WebKit launches a NetworkProcess at all (the launcher's
`PROCESS:LAUNCH:network` marker answers it). (2) If it does, check that one launched for a non-persistent store
touches no profile path. (3) If it cannot be avoided, a light view shares the full views' NetworkProcess, which
stays idle for it, and the 107 MiB saving is dropped from the claims. Either way light behaviour is enforced
in webd and the light interface, not by hoping the engine stays idle.

**Configuration a light view cannot widen.** `LightConfig` has `scripts`, `color_scheme`, `default_font_size`,
`user_style` (extra CSS from the caller) and nothing else: no `origins`, no `profile`, no scheme policy. Image
and font bytes in the page come from `data:` URLs the caller wrote (Mail rewrites `cid:` parts; Docs inlines or
omits images, as it does today). `base_url` may be `about:blank` or an opaque origin; a `file:` base is refused
(open question 5).

## 4. The API (MIDL sketch, `idl/webd.midl`)

Follows `display.midl`: calls are request/reply, events are `oneway` on the endpoint the client moved in
`CreateView`. Ids append-only from day one. The light interface (`os.lazy.webd.light.v1`, 3.6) is the subset
marked *(L)* below with a `LightConfig` in place of `ViewConfig`; both are generated from one set of shared
structs so the client code is shared.

```
interface os.lazy.webd.view.v1 {
    // (L) = also on os.lazy.webd.light.v1 (CreateView takes a LightConfig there)
    method CreateView(config: ViewConfig = 1, width: U32 = 2, height: U32 = 3, scale: U32 = 4,
                      events: Channel<os.lazy.webd.view.v1> = 5) -> (view: U64 = 1) = 1;
    method DestroyView(view: U64 = 1) -> () = 2;
    // (L) AttachSlot, ReleaseSlot, Resize, SetVisible, LoadHtml, Mouse, Wheel, Key, Focus, DestroyView;
    // (L) events: Frame, Band, State, LinkClicked, ClipboardWrite, ViewLost
    method AttachSlot(view: U64 = 1, slot: U32 = 2, pixels: Buffer = 3) -> () = 3;
    method ReleaseSlot(view: U64 = 1, slot: U32 = 2) -> () = 4 oneway;
    method Resize(view: U64 = 1, width: U32 = 2, height: U32 = 3, scale: U32 = 4) -> () = 5;
    method SetVisible(view: U64 = 1, visible: Bool = 2) -> () = 6 oneway;
    // navigation
    method Load(view: U64 = 1, url: String = 2) -> () = 10;
    method LoadHtml(view: U64 = 1, html: Buffer = 2, base_url: String = 3) -> () = 11;   // the app serves its own content
    method Reload(view: U64 = 1, bypass_blocking: Bool = 2) -> () = 12;
    method Stop(view: U64 = 1) -> () = 13;   method Back(view: U64 = 1) -> () = 14;   method Forward(view: U64 = 1) -> () = 15;
    // input (oneway)
    method Mouse(view: U64 = 1, kind: U32 = 2, button: I32 = 3, x: I32 = 4, y: I32 = 5, mods: U32 = 6) -> () = 20 oneway;
    method Wheel(view: U64 = 1, x: I32 = 2, y: I32 = 3, ticks_x: I32 = 4, ticks_y: I32 = 5, mods: U32 = 6) -> () = 21 oneway;
    method Key(view: U64 = 1, press: Bool = 2, keysym: U32 = 3, text: String = 4, mods: U32 = 5) -> () = 22 oneway;
    method Focus(view: U64 = 1, focused: Bool = 2) -> () = 23 oneway;
    // answers to events
    method DialogReply(...)  method NavigationAnswer(...)  method NotifyPermission(...)  method DownloadControl(...)
    // later, capability-gated: Find, Zoom, Snapshot(-> Buffer), PrintToPdf, EvaluateScript (dev scope only)
    // events (webd -> client): Frame, Band, State, Hover, Cursor, Dialog, NavigationRequested, NewWindowRequested,
    //   Download{Started,Progress,Done,Failed}, Notify{Show,Cancel,PermissionRequest}, Responsive, ViewLost(reason)
    struct ViewConfig { profile: String = 1, ephemeral: Bool = 2, scheme_policy: SchemePolicy = 3, origins: Array<String> = 4,
                        scripts: Bool = 5, handoff: Handoff = 6, downloads: Mode = 7, popups: Mode = 8,
                        blocking: Bool = 9, notifications: Bool = 10, user_agent_suffix: String = 11 }
}
```

Client surface, in the order of effort:

* **`xui-app/crates/webview`**: `WebView` xui widget plus `RemoteEngine: xui_webkit::Engine`. `WebView::new(opts)`;
  `.load(url)`, `.load_html(html, base)`, events as xui messages. LazyWeb+'s window changes the engine it is
  constructed with and nothing else.
* **Rhai**: `midlc --rhai-api` generates `sys::webd::*` for scripts for free; frames are not meaningful there,
  so the LazyRAD **`WebView` control** wraps the widget (`web1.load("https://...")`, `web1_on_title`).
* **`os.lazy.webd.browser.v1`** and **`os.lazy.webd.net.v1`** scopes (not interfaces): consent text for "keeps
  cookies" and for "loads content from the internet", see section 5.

## 5. Security model

Trust boundaries: page process -> webd (exists: IPC from a compromised WebProcess); client app -> webd (new);
webd -> user (same uid as clients; the label, not ownership, is the barrier).

| Threat | Mitigation |
|---|---|
| App without `network` gets the internet through webd | A view's `net` is `None` unless the caller holds `os.lazy.webd.net.v1`; `None` = `data_only` scheme policy, network process never consulted. The consent screen lists the scope |
| App reads another app's cookies or storage | Profile = kernel-stamped label of the caller, never a wire field; `browser` profile needs its scope; web processes per profile |
| Mail/Docs content phones home or runs hostile script | A light view has no network path in webd or in the interface it was created through, no storage, scripts off by default, links reported not followed (3.6); a hostile-message test in the Mail harness requires an empty host request log |
| Page makes the client start a program | Handoff is decided in webd; `ask` is a hold-and-deny-on-timeout event; `mimed` is reachable from webd only with the browser scope. The 3 s real-input lock stays where the navigation is decided |
| Client floods webd (views, events, buffers) | Per-caller caps: 8 views, 4 MiB-class event queue, slot size bound to the screen, input rate cap, 256 downloads; memory charged to the user quota (same uid) and webd reads the charge (`lzw_ui_user_memory`) before creating a process |
| Malicious slot buffer | Client-owned pages are mapped read/write by webd: size checked against `buffer_map`, every paint rectangle clipped to the view, slot state machine refuses a paint into an unreleased slot |
| Compromised page process | Unchanged: confined labels, no interface, fonts only. The UI-process side gains a per-view identity check on every `lzw_ui_*` coming from WebKit |
| Compromised network process | Can write all profiles (section 3.3). Stated, not hidden |
| Client lies about input to defeat gestures | A client can already act as the user in its own window; the gesture lock only bounds what **page script** can do, which still needs real forwarded input per hand-off |
| Local files | webd's children read no user file. Apps pass content with `LoadHtml`; `file:` browsing in LazyWeb+ becomes "client reads, passes bytes + base URL" (open question 5) |

## 6. What is shared and what is not

Shared: one UI process, one NetworkProcess, the HTTP cache and DNS/TLS state, fonts and the Fontconfig cache, one
compiled ad-block store and its updater, one crash handler/heartbeat/shutdown path, the WebProcess pool's spare.
Not shared: cookies, storage, history, web processes across profiles, JS heaps.
Arithmetic (*est.*, 107 MiB per process): two browsers today = 2 x (UI + net + web) = 642 MiB; with webd and
**views of one group (an owner's windows of one profile) on one web process** = UI + net + web = 321 MiB while
the group lives on its first process (measured: two views, and a third joining after the first closed, `webkit-views group`); add Mail's ephemeral group = +107 MiB (its own web process, shared by every
Mail pane). **Without grouping** each view is +107 MiB (measured: `webkit-views share`), so grouping is what makes
the arithmetic true. A lone browser is not cheaper than today (+ the daemon's own thread and a hop); the gain
starts at the second consumer and at the second browser window. Three further levers cut the 107 MiB itself:
smaller images (ICU's data is about 45 MiB of it), and sharing text pages between the processes of one program
(issue #942). A related page shares the process and the data store, so a **group is one owner's and one
profile's**: webd groups by (client label, profile), never across clients.

## 7. Phases

### P0 spike results (2026-10-21)

| Spike | Result |
|---|---|
| (a) an app serves, another resolves | **Done, no kernel change.** The kernel already enforces and tests `app.<id>.<name>` plus own-domain interfaces; the client rule was the gap and is one `SERVICE_NAMES` row per interface with a host test (section 3.1). A guest check comes free with the P2 harness, and the LazyCal branch exercises the same path first |
| (c) paint into another process's Buffer | **Feasible by reading.** `WKPagePaint(page, void* dest, size, rect)` writes into any pointer (`paint.cpp`, `smoke/main.cpp`); a mapped slot of `width * 4` stride is such a pointer. WebKit paints BGRA, so `Frame` carries a `format` (the display's `set_layout` already does this). Not run |
| (b) light view: no network process; two stores in one host | Not run: needs the multi-view engine (P1). Plan assumes the pessimistic answer (a light view still starts a network process that stays idle) until L1 of section 3.6 shows otherwise |
| (d) hop latency | Not run: needs a QEMU session with a stub service. Moved to the P2 acceptance criteria (section 3.4: within 10% of today on the scroll repro) |

### Phase table

Order changed by the light view (3.6): Mail and Docs need only light views, so **Blitz can go before the full
browser moves onto webd**.

| Phase | Deliverable | Exit criteria | Size *est.* |
|---|---|---|---|
| **P1.0 remove webkit-app** (done) | Section 9.1: delete `os.lazy.webkit`, its window feature, its harness; fold what only `app_run.py` checked into `tools/webplus/run.py` | `webplus/run.py`, `notify_run.py`, `hang_run.py`, `attack_run.py --browser --app lazywebplus` and `--kernel` pass (run 2026-10-21 on the merged branch); `python tools/test_suites.py`, `test_catalog*.py`, `cargo test -p build-support-tests` pass | 2-3 days |
| **P1 multi-view engine** (done) | `Host`/`View` split in `webkit/port/app` behind a `ViewSink` and a `HostEnv`; the C ABI of `lazyos_ui.h` becomes `legacy.cpp` over view 1 (**it did not grow view ids**: webd implements the same two interfaces over Messenger in P2); `webkit/port/views` (`webkit-views`) runs a light view, two views and a cycle of twelve in the build container | `port.py --only WebProcess NetworkProcess webkit-views --run-views` passes (done, ten runs in a row); `webplus/run.py`, `notify_run.py`, `hang_run.py` and the attack harness judge unchanged (run on the merged branch) | 1.5-2.5 weeks |
| **P2 webd, light first** (slices 1 and 2 done; 3 is the full interface) | `idl/webd_light.midl` then `idl/webd.midl` (+ `explain` rows), `xui-app/crates/webd`, `webd` bin, package `xui-app/packages/webd` (resident, autostart), `LAZYOS_WEBD=1`, `run_demo.py --webd`, GUI control, `tools/webd/run.py`, a demo client | two apps with light views painting, no request in the host log, no network process (if L1 allows), `ViewLost` after killing a web process, hop latency measured; then the full interface: two profiles, cookies isolated | 2-3 weeks |
| **P3 Blitz out** | Section 9.2: Mail's pane, then Docs, then `xui-blitz`/`webfonts` from those; LazyWeb waits for P4 | per step in 9.2 | 1 week |
| **P4 LazyWeb+ thin client, LazyWeb out** | `xui-webplus` on `RemoteEngine`; downloads, dialogs, notification bar and blocker UI keep their code, their engine halves move; tabs possible; then LazyWeb deleted | `webplus/run.py`, `notify_run.py` pass against webd; the 108 MiB engine host leaves the package | 1.5-2.5 weeks |
| **P5 hardening** | process caps, memory-pressure eviction, background throttling, fairness, reconnect protocol, attack harness for webd (malicious client, malicious page process), seeded fuzz of the slot state machine and `ViewConfig` decode | no `LABEL:DENY` beyond the documented optional ones; two views scrolling at once keep frame pacing | 2 weeks |
| **P6 optional** | compositor subsurface; PDF/print and snapshot API; LazyRAD `WebView` (light by default); web-app package kind | measured | open |

### P2 slice 1: light views, headless (started 2026-10-21)

The first slice builds the light interface end to end up to, but not including, the program and package. Two
halves written against one contract, `webkit/port/webd/webd_abi.h` (C ABI, light views, whole frames only):

* **Service half (Rust)**: `idl/webd_light.midl`, the `pkgstore::explain` row, `xui-app/crates/webd` (view table,
  quotas, light config validation, slot state machine, the Messenger service on its own thread), host-tested
  against an in-memory fabric and a recording `Engine` trait. An `ffi` module (feature `host`) implements that
  trait over the C ABI and exports the `webd_ui_*` callbacks; it is not linked into anything yet.
* **Engine half (C++)**: `webkit/port/webd/abi.cpp` implements `webd_engine_*` over the engine's Host and Views,
  with the light-view behaviour the engine lacks (scripts off, `LoadHtml` with a base URL, navigation locked and
  reported as `webd_ui_link_clicked`, no network scheme loads, no browser clients), and `webd-abi-test`, a
  headless program in the build container that drives the ABI like the service will (modes under
  `tools/webkit/port.py --run-views`' sibling flag).

**As built, service half** (`idl/webd_light.midl`, `xui-app/crates/webd`; host tests only, nothing is linked):

* One interface for both directions, like the display protocol: calls and oneway input, and the events
  (`Frame`, `State`, `Scroll`, `LinkClicked`, `ClipboardWrite`, `ViewLost`, plus a `Ping` liveness probe) on the
  endpoint moved in with `CreateView`. Ids: calls 1-7, 11, 20-23; events 100-106. `midlc` has no imports, so
  the shared structs of the full interface will be declared again in `webd.midl` (same ids and field numbers;
  `CreateView` leaves field 4, `scale`, unused in the light one).
* Numbers (`limits.rs`, each with its reason): 8 views per client, 1..=8192 per side and 16 Mpixel, 3 slots per
  view, 512 MiB of declared slot memory per client, `html` at most 8 MiB, `user_style` at most 64 KiB; events cut
  text at 48 KiB and the host must offer a 128 KiB receive buffer. Refusals: `EINVAL` (size, config, base URL,
  slot too small), `ENOSPC` (views), `EDQUOT` (slot memory), `E2BIG` (page), `EBUSY` (slot in use), `ENOENT`
  (not your view, same as no view), `ENODEV` (the engine lost the view), `EACCES` (another login session).
* Identity: the caller is `(uid, label)` from the kernel stamp (an unlabelled caller is told apart by task and
  shares nothing); the `group` handed to the engine is a number the service allocates per client and never
  reuses, so one client's views share a web process and two clients' never do.
* Slots: a frame goes to the lowest free slot or is dropped; a dropped frame marks the view `dirty` and the
  next `ReleaseSlot` (or `AttachSlot`) asks the engine for a whole frame (`webd_engine_refresh`), so the client
  ends on the latest picture without the service holding frames. `Resize` detaches every slot; the client
  attaches new ones. The engine's callbacks fill an `EventQueue` that keeps only the latest frame, state and
  scroll per view; `Service::pump` runs on the service thread.
* Engine loss: `ViewLost` keeps the table entry (quota held) until `DestroyView`; calls on it answer `ENODEV`.
  A client that stops answering `Ping`s (`EPIPE`) loses its views.

Slice 2 links the two into the `webd` program, adds the package, `LAZYOS_WEBD=1`, `run_demo.py --webd`, the GUI
control, `tools/webd/run.py` and a demo client; slice 3 is the full interface.
### P2 slice 1, engine half: done (2026-10-21)

`webkit/port/webd/abi.cpp` implements every `webd_engine_*` of `webd_abi.h` over the engine, with the light-view
behaviour in `webkit/port/app/light.cpp`; `webd-abi-test` (`port.py --only WebProcess NetworkProcess webd-abi-test
--run-webd`) plays the service and passes its eight modes in the build container (`html script config lock network
many lost cycle`), with `webkit-views` unchanged. Details and measurements: `webkit/README.md`, "webd's engine half".
What it found:

* **The network lock is a content rule list, not the navigation policy.** The policy sees navigations only; a light
  view with scripts on and no rule list made 26 connections to a listener (images, style sheets, `@import`, scripts,
  fetch, XHR, WebSocket, `import()`, ...); with it, none, scripts on or off. The list is compiled once at start into a
  folder the engine writes (`$LAZYOS_WEBD_RULES_DIR`, else `/tmp/webd-rules-<pid>`), so webd's label needs one
  writable folder, and the first view waits for the compile.
* **`color_scheme` is not implementable here**: `prefers-color-scheme` is compiled out (`ENABLE_DARK_MODE_CSS` is off
  and turning it on rebuilds WebCore). The engine says `CONFIG:IGNORED:color_scheme`; a caller that wants a dark page
  passes `user_style`. `user_style` is appended to the document (the C API has no user style sheet);
  `default_font_size` is applied (default and minimum).
* **A page's navigation cancels its own scheduled one** (a refused click before a meta refresh fires loses the
  refresh); a relative URL against `about:blank` is told as it is, not absolute; `<noscript>` is not rendered with
  scripts off.
* **The mouse `kind` of `webd_abi.h` (0 move, 1 press, 2 release) is not the engine's** (0 down, 1 up, 2 move, as
  `xui_webkit::MouseKind` and `lazyos_ui.h`): `abi.cpp` translates; the Rust half must send the header's numbers.

### P2 slice 2: the program, the package and the switch: done (2026-10-21)

`webd-app` joins the two halves (`webkit/port/webd/main.cpp`, `xui-app/webd`), the package `os.lazy.webd` is
resident and opens with every session of a `LAZYOS_WEBD=1` image, and `run_demo.py --webd`, the launcher's
Advanced tab, `tools/webkit/webd.py`, `docs/build-switches.md` and `tools/suites.toml` (`test_only`) carry the
switch. As built and measured: [`webd.md`](webd.md). What it found:

* **The first thread must run the service, not WebKit.** `init`'s lifecycle `Watch` is keyed by the sender's
  task slot (`row.pid == message.sender`), and a thread other than the process's first carries its own slot, so
  it is `ESRCH` (the same fact as the notification service's, `lazywebplus.md`). webd therefore runs the
  service on the process's first thread and WebKit's run loop on a thread `main.cpp` makes; the engine's calls
  are queued tasks and its callbacks run on that thread, so which thread is "main" does not matter to it.
  (If WebKit had to stay on the first thread, the lifecycle channel would be a main-thread handle watched by an
  endpoint descriptor and read through `lzw_engine_run_on_main`, as the shell's toast channel is.)
* **Wake-up** is one `wait_any` on the endpoint, the lifecycle channel and an `eventfd` (`WAIT_FD`), with a
  one-second deadline for the `Ping`s and the watchdog; `docs/webd.md` has the design and the numbers.
* **No `webd_abi.h` change for engine loss.** The per-view loss is the engine's `webd_ui_view_lost`; the
  engine as a whole is lost when WebKit's main thread stops turning, which the service detects itself (a probe
  task through `webd_engine_run_on_main`, unanswered for 20 s: `Service::engine_lost`, exit status 4). A
  "network process ended" callback was dropped: WebKit restarts it on demand and a light view does not use it.
* **`rhai` cannot create a view** (`CreateView` carries a channel, `LoadHtml`/`AttachSlot` a buffer, and a script
  cannot send either), so the proof of a client round trip is webd's built-in client (`Reopen("/selftest")`),
  which uses the real road from a thread of the daemon. A real client app is `webddemo` (another branch).
* **Contract notes** for the next slice: `webd_abi.h` says the view-start calls run "on the process's main
  thread"; read it as "the thread that runs the loop". `Service::new` takes the session as a number the host
  reads with `cred_get`; a second login session's webd is still not supported (section 3.1).

* **Measured** (WHPX, [`webd.md`](webd.md) "Measured"): idle webd costs about 0 % CPU and **5.4 MiB**, not the 108 MiB
  the plan assumed (the program is demand-paged; section 8's "idle cost is the program image" is wrong by
  20x); a view that is up adds about 28.7 MiB (web process and network process) and costs no CPU while it sits;
  the first view takes about 1 s (the web process starts), later loads and `Refresh` come back in 32-34 ms, which
  is the engine's 33 ms paint pacer, so the plan's hop-latency criterion (3.4) still wants a probe that does not
  paint.
* **Permissions derived** ([`webd.md`](webd.md) "Permissions"): `os.lazy.webd.light.v1` and one writable folder;
  no `LABEL:DENY` for webd's own label in any run, and for its children only the optional ones LazyWeb+
  documents. The NetworkProcess runs with no socket and no file (`network = []`, `files = []`).
* **Lifecycle**: `init`'s `Quit` and `SIGTERM` both end it cleanly (`WEBD:QUIT:PASS`, exit status 0, no engine
  process left); started again, the name registers again; the shell's tray item follows it.

### What P1 found (2026-10-21, `webkit-views`)

* **One NetworkProcess, one WebProcess per view.** Two views with different stores, and two views of one profile
  directory, each launched a web process, and all shared one network process. **A view costs 107 MiB, not a
  profile**: decision D4's "web processes are never shared across profiles" is a floor, and section 6's arithmetic
  (one web process per profile) is too kind. A light view
  launches the network process too (L1, above). A destroyed view's web process ends with it, unless another view
  of its group uses it.
* **Related pages share a WebProcess** (`ViewConfig::group`; `WKPageConfigurationSetRelatedPage`, which also
  needs the group's one data store): `webkit-views group` and `groupprof` launch one web process for two views,
  destroying one leaves the process to the other (the closed page is unloaded to `about:blank`: it must not keep
  running in the process the others use), and a view that joins after the first member is gone finds a live
  one. Different profiles under one group number do not share (`groupmix`).
  **A group's process cannot be recovered in place.** The sharing is made when the page is made, not when its
  process starts: ending the process (a hang the owner recovers, or a crash) and reloading its pages launches a
  process per view (measured: two views, 1 launch, recover, 3 launches), and in most runs it aborted the UI
  process (`WebPageProxy::launchProcess` -> `WebProcessProxy::removeWebPage` -> `updateMediaStreamingActivity`: a
  run-loop timer with a CheckedPtr to a dead object, the family of patches `0021` and `0027`; reloading one at a
  time did not help). So `recover` on a process other views use is refused (`WEBPROCESS:RECOVER:SHARED`), and
  webd recovers a group by **destroying its views and making new ones**: the last destroyed view ends the
  process, a new view of the group starts exactly one (`group` proves it: 2 launches in all). A real crash of a
  shared process still reaches every view as `crashed`, and a reload of each starts a process each: webd's
  answer is the same (recreate), and P5 measures how often it happens. Not measured yet: a group's memory after
  many navigations (one process holds every page's heap). Process swapping on navigation is off for the whole
  context (`engine.cpp`), so a cross-site navigation does not split a group.
* **A WebPageProxy cannot be destroyed on this port.** `webkit-views two` aborted the UI process one run in three
  after the first view was released (a CheckedPtr to a dead run-loop timer inside `didReachLayoutMilestone`; the
  family of patches `0021` and `0027`); `WKPageClose` first made it worse; ending the page's process and parking
  the hollow proxy passes ten runs in a row. For webd this means a client's view that comes and goes (Mail's
  message pane) leaves a proxy behind each time (and the store and session of a private view): bitmaps, the pacer
  thread and the process are freed. **P5 needs a pool of parked views reused by the same owner and profile**, or a
  WebKit patch that finds the dead timer.
* **Three of the page callbacks are process-wide** (patches `0024`, `0028`, `0030`): fixed in the engine by finding
  the view from the page argument. A patch that makes them per page would be cleaner and costs a WebKit rebuild.
* **Memory-only and persistent owners cannot share a NetworkProcess's curl jar.** A `memoryOnly` view (#940) sets
  `CURL_COOKIE_JAR_PATH=:memory:` and the children's folder for the whole process; a persistent profile exports its
  own once. With both in one host the children's environment depends on which view was made first, and a running
  NetworkProcess has read it already. For LazyWeb+'s single view this equals the old behaviour. For webd it needs
  the children's environment decided once at host start (or a second NetworkProcess for memory-only owners): P2.
* **Reserve the webd names.** `pkgstore` lets a client resolve `app.os.lazy.webd.*` to whoever holds the label
  `app:os.lazy.webd`, and nothing yet stops a user package from being named `os.lazy.webd` on an image that does
  not ship the core one. P2 has `pkgd` refuse user packages in `os.lazy.*`.
* **The children's environment is the first persistent profile's** (`LAZYOS_WEBKIT_PROFILE`, `CURL_COOKIE_JAR_PATH`):
  a second profile's cookies in a shared NetworkProcess need a patch or a second process (decision D4's "per-profile
  cookie jars at first" is true for the data store, not yet for curl's jar).

Kernel testing requirement: nothing above adds kernel code. webd's correctness tests are host tests (model
against the recording engine and an in-memory fabric); its stress tests are harness runs of thousands of view
create/destroy cycles judging descriptors, frames and the user-memory charge (`hang_run.py` already judges that
charge for ended processes).

## 8. Failure and lifecycle

* **Start and stop.** webd is a resident app with `autostart = true` (the LazyCal precedent): it starts at login,
  creates the WebKit context only when the first view arrives, and has no idle exit in v1. Idle cost
  (measured in P2 slice 2, `webd.md`) is about 5.4 MiB and no CPU: the program is demand-paged, not loaded whole;
  this is the price of making it mandatory for Docs and Mail (section 9.2). If the user quits it from the tray, or `init` does not restart a failed launched app, the name no
  longer resolves: the client library shows "The web engine is not running" with a *Start* button
  (`init.Launch`, P2 checks the permission it needs), never a blank view.
* **webd's UI process crashes** (history above): every client gets `ViewLost(reason)` when its channel closes;
  the client library reconnects and the widget reloads the last committed URL (a light view: its last
  `LoadHtml` content, which the client library keeps), as the window already does for a lost page process.
  Sharding the host per profile (several `webd-host` processes behind a broker) is the escalation, and additive.
* **A client dies**: its channel closes, webd destroys its views, ends its web process if nothing else uses it,
  frees slots. The display's `EPIPE` liveness approach is enough.
* **Shutdown/logout**: `init` sends `Quit` (resident lifecycle); webd ends children with the existing
  SIGTERM/SIGKILL-by-group path, flushes profiles, answers within the 3 s grace.
* **Memory**: `lzw_ui_user_memory` gates process creation; refusal surfaces as `ViewLost(no_memory)`.

## 9. Removal plans

### 9.1 Removing `webkit-app` (`os.lazy.webkit`, W5) first (done)

**Done** in this branch. What differed from the inventory below: no check had to be folded into
`tools/webplus/run.py` (it already covered HTTPS pages with SNI, the typed form and Back, script, the wheel,
resize and maximize, and the reaping of both children); `--plus` is gone from `tools/webkit/app.py` (it builds
LazyWeb+ only), so `LAZYOS_WEBKIT_APP_ID` and `LAZYOS_APP_DATA_DIR` left the C++ build and `engine.cpp` has no
default profile (a window that asks for none gets an ephemeral store); `build_support/webkit_embed.rs` went and
`engine_source` moved to `lazywebplus_embed.rs`; the page-view tests that drove the W5 window now run in a rig of
their own (`xui-app/crates/webkit/tests/support`), and `tests/window.rs` (chrome only) was deleted; the kernel,
`filepolicy` and `pkgstore` fixtures named `os.lazy.webkit` became `os.lazy.lazywebplus`; the sandbox attack
harness attacks LazyWeb+'s labels only, and the kernel-only driver now creates `~/Downloads` because the network
process's table includes it. Patch `0015` still names `os.lazy.webkit` in a comment: editing an applied patch
rebuilds all of WebKit, so it is left.

**Upgrading an existing OS volume.** `pkgd`'s provisioning demotes a core package a newer image no longer
ships to a user app instead of removing it (`libs/pkgstore/src/provision.rs`, `Action::Demote`), so a volume that
already had `os.lazy.webkit` keeps it in `/apps` and the menu until it is uninstalled or the volume is reset
(`--reset-os`). That is existing behaviour for every retired core app; say so in the PR rather than add a
one-off retire step.

Why first: W5 is LazyWeb+ minus chrome, `lazywebplus.md` already says the retirement was held back only for
lack of a page-for-page harness run, and the multi-view refactor would otherwise port two windows to the new ABI.

Inventory (from `git grep`; delete or shrink, nothing else depends on it):

| Area | Change |
|---|---|
| Package and window | delete `xui-app/packages/webkit/`; in `xui-app/crates/webkit` remove the `window` feature (`app.rs`, `chrome.rs`, `window_ffi.rs`, the `[features]` entry) and keep the seam (it becomes the client's base); drop the defaults in `webkit/port/app/main.cpp` and `log.cpp` that only W5 used (the `WEBKIT:W5` prefix) |
| Build | `tools/webkit/app.py`: drop the non-`--plus` path (the flag becomes the default and is then removed); `tools/xui/core_packages.py`, `build_support/webkit_embed.rs`, `xui_embed.rs`, `core_packages.rs`, `linuxapps_embed.rs`, `os_disk.rs`, `os_track.rs`, `build.rs`: remove `LAZYOS_WEBKIT` (keep `LAZYOS_WEBKIT_PORT` for the engine children and `webkit-smoke`); `PlatformLazyOS.cmake`: one UI target |
| Launchers | `tools/run_demo.py`, `demo_args.py`, `demo_builds.py`, `lazygui/{appsteps,osvolume,ui,variables}.py`, catalog tests (`test_catalog_webkit.py`, `test_run_demo_webkit.py` deleted), `tools/suites.toml` (`full`), `test_suites.py`; `--webkit` removed, `--lazywebplus` kept |
| Harness | delete `tools/webkit/app_run.py`, `app_session.py`, `appjudge.py`, `test_app_judge.py`, the `webkit.json` session; first move what only they checked (the `WEBKIT:W5:*` HTTPS/SNI/form/Back/wheel checks that `webplus/run.py` does not repeat) into `webplus/run.py`; `attack_run.py --app` keeps `lazywebplus` only (`attackjudge.py`, `attack_build.py`, `sandboxprobe.rs`, `test_attack_judge.py`) |
| Labels and tests | `kernel/src/ipc/confine.rs`, `confine_suite/*`, `filepolicy/src/tests.rs`, `pkgstore/src/filerules.rs` use `os.lazy.webkit` as an example label: rename to a neutral fixture id, keep the cases |
| Docs | `docs/webkit-port-plan.md` W5 becomes history, `webkit/README.md` "The desktop app (W5)", `docs/lazywebplus.md` "`os.lazy.webkit` is not retired", `docs/build-switches.md`, `docs/architecture/process-sandbox.md` diagram, `THIRD_PARTY.md`, `AGENTS.md` ("The WebKit browser" section) |
| Fixtures | `tools/web/fixtures/theoldnet.com/webkit/` pages: keep (webplus uses the stand-in sites) |

Done when: no tracked file names `LAZYOS_WEBKIT=` (only `LAZYOS_WEBKIT_PORT`), the listed harnesses and host tests
pass, and the `--lazywebplus` image behaves as before.

### 9.2 Removing Blitz

Blitz is `xui-blitz` (a git dependency of `va1erian/xui`, pinned at one revision in four `Cargo.toml` files) plus
`xui-app/crates/webfonts`. Consumers, each needing a web view with the same behaviour:

| Consumer | What it needs from webd | Order |
|---|---|---|
| **Mail** reading pane (`xui-app/mail/src/app/reader.rs`) | a **light view** with scripts off (the defaults of 3.6): `LoadHtml` of the message after Mail rewrites `cid:` parts to `data:` URLs, links reported as `LinkClicked(href)` and never followed, `ClipboardWrite(text)` for Copy, light/dark `color_scheme`. Mail already refuses remote content, which a light view cannot load anyway | 1st: smallest, the hardest sanitising rules |
| **Docs** (`xui-app/docs`) | a light view: `LoadHtml` of the Markdown page `docs/src/lib.rs` already produces, the same link and clipboard events, `Find`, text zoom; the Markdown converter stays | 2nd |
| **LazyWeb** (`xui-app/web`, `LAZYOS_LAZYWEB=1`) | nothing: it is deleted. LazyWeb+ already has its menus, history, downloads and keys and becomes the default `mimed` handler | after P4, since it is the only consumer of Blitz's fetcher |
| `webfonts`, `xui-blitz` dependency | none | last |

Mail and Docs need no full view, no profile and no network scope, so **P3 does not wait for LazyWeb+ to move**.

Deleting LazyWeb also takes care of what `tools/webplus` borrows from it: `tools/web/judge.py` (loaded by
`tools/webplus/judge.py`), `tools/web/sites.py`, `tools/web/certs.py` and `tools/web/fixtures/` are LazyWeb+'s
harness inputs too and must move (to `tools/webplus` or a shared `tools/webtest`) before `tools/web/run.py` goes.

Per-step exit criteria: Mail: `python tools/mail/run.py` judges the same card/body checks with the web view (pixels
and `cid:` image), and a hostile message (script, remote image, `<meta refresh>`, form post) produces no request in
the host log; Docs: `xui_docs.json` and `xui_docs_open.json` pass (markers kept); LazyWeb: `python tools/web/run.py`
is replaced by `webplus/run.py` (its fixtures stay, `tools/web/sites.py` is shared); `lazyweb.json` session,
`--lazyweb`, `LAZYOS_LAZYWEB`, `xui-app/packages/lazyweb`, `docs/lazyweb.md`, the `mimed` registrations and the
Settings > Default apps entries go; `xui-app/Cargo.lock` loses the git dependency; `webfonts` and
`docs/xui-docs.md`'s Blitz sections are removed or rewritten.

What this costs, stated plainly:

* **Weight.** LazyWeb, Docs and Mail's pane run in about 12 MB each today. After this change they need webd
  (about 108 MiB always resident) and, when a view exists, a web process (+107 MiB); a light view adds a network
  process only if L1 (3.6) fails. Mitigations, in the order to try them: the lazy WebKit context (in the plan), L1,
  file-backed `mmap` with shared text (the kernel item that matters most), a split of webd into a small broker and
  an engine host that exits when idle.
* **Failure coupling.** A webd crash blanks the reading pane and Docs. Hence the reconnect path (section 8) is P2,
  not P5.
* **Licences.** LazyWeb is `GPL-2.0-only`; its rules (`nettls/licenses.py` GPLv2 checks, the AGENTS.md "never link
  any of it into LazyWeb" sentence) disappear with it; the other side of the firewall (clients link no WebKit)
  stays.
* **Fonts.** Blitz needed `webfonts`; webd's children use Fontconfig and the Liberation fonts already in the image.

## 10. Conventions checklist (AGENTS.md)

MIDL files for the interfaces (`idl/webd_light.midl`, `idl/webd.midl`, `idl/manifest.json`, `pkgstore` explain
rows, generated stubs, no hand-written ids); a `Webd` row in `libs/tracefmt` channels and
`docs/trace/channels.md`, every marker through `xui_app::trace`, `launch::run(Channel::Webd, ...)`; paths from
`libs/fhs`; files under 500 lines (the C++ split will touch `engine.cpp`, `clients.cpp`, `paint.cpp`);
launcher/GUI wiring in `tools/lazygui/catalog.py` with `test_catalog.py`, `run_demo.py --webd`,
`tools/suites.toml`, `tools/xui/core_packages.py`, permissions derived under `LAZYOS_LABEL_TRACE=1`; licence:
webd links WebKit (LGPL-2.1) and OpenSSL, so it is GPL-3.0-or-later, and the process boundary is a licence firewall
(clients may be any licence as long as they link none of it); `python tools/check_file_length.py`,
`check_adhoc.py --ratchet`, the pre-PR review of AGENTS.md.

## 11. Open questions, answered the simple way

Each was a fork; this is the path taken. Change one only if a spike or a measurement contradicts it.

| # | Question | Decision | Revisit when |
|---|---|---|---|
| 1 | Naming of an app-served interface | `os.lazy.webd.view.v1` as `app.os.lazy.webd.view` and `os.lazy.webd.light.v1` as `app.os.lazy.webd.light`, a `SERVICE_NAMES` row each; no kernel change (P0a) | the LazyCal generic rule lands: use it |
| 2 | Activation | resident, `autostart = true`, no on-demand start, no idle exit; the client library shows "not running" + *Start* | idle memory is a complaint: split broker/host |
| 3 | Does a light view need the network process | assume it does until L1 (3.6) in P1 | P1 measures it |
| 4 | Hop latency budget | within 10% of today on the Ars Technica scroll repro; measured in P2 | P2 misses it: subsurface (P6) moves up |
| 5 | Local files | the client reads and passes bytes with `LoadHtml(base_url)`; webd's children read no user file | a use case needs relative file loads |
| 6 | Notification identity | webd posts the toast under its own name with the site host as title | the shell grows `on_behalf_of` |
| 7 | Web processes per profile | one per (client app, profile) **group** (`ViewConfig::group`, P1; **webd computes the number from the kernel-stamped client label and the profile, never from a wire field**: the engine cannot tell owners apart), hard cap of 4 processes per user, refusal is `ViewLost(no_memory)` | P5 measures real use |
| 8 | Remove W5; relicense the client crate | W5 goes first (9.1); the client crate stays GPL-compatible until a maintainer relicenses | someone needs an MIT client |
| 9 | Do light views of different apps share a web process | no: one per client app; HTML and image decoders are the attack surface even with scripts and network off | memory forces it and the content is trusted |
