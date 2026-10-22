# webd

`webd` is the per-session daemon that owns WebKit and gives other apps **light views** (an app hands it a page
of HTML and gets the page's pixels back in shared memory, plus a few page events) and **full views** (what a
browser needs: navigation over the network, a profile, dialogs, downloads, notifications, content blocking,
frames that carry only what changed). Plan, security model and the phases: [`webd-plan.md`](webd-plan.md). This
file is the as-built description of the program (P2 slices 2 and 3) and of its clients: Docs and Mail (P3:
`xui_app::lightview`, [`xui-docs.md`](xui-docs.md), [`mail.md`](mail.md)) on light views, the test client
Web View Test on full views ("Full views", below). The interfaces are
[`idl/webd_light.midl`](../idl/webd_light.midl) (served as `app.os.lazy.webd.light`) and
[`idl/webd.midl`](../idl/webd.midl) (`os.lazy.webd.view.v1` and its scopes, served as
`app.os.lazy.webd.view`), the portable service is `xui-app/crates/webd` (`xui-webd`), the engine half is
`webkit/port/webd/abi.cpp`, `abi_full.cpp` and `sink_full.cpp` ([`webkit/README.md`](../webkit/README.md),
"webd's engine half").

## What runs

```
 client app                                 webd (os.lazy.webd, one per session, resident, no window)
 (any label that lists                       first thread: the service (Rust, xui-app/webd)
  os.lazy.webd.light.v1)  --- Messenger --->    registry name app.os.lazy.webd.light, init's lifecycle line
        ^   CreateView / LoadHtml / input        xui-webd: view table, quotas, slot state machine
        |   events on the channel it moved in            |  webd_engine_*  (queue work, never wait)
        '-- Frame(slot) / State / LinkClicked ...       v
                                                  second thread: WebKit's run loop (C++, abi.cpp over the engine)
                                                         |  fork+exec, confined labels
                                              app:os.lazy.webd/webprocess (one per client app and profile, fonts only)
                                              app:os.lazy.webd/networkprocess (one for every profile)
```

* **Package** `xui-app/packages/webd` (`os.lazy.webd`): `resident = true`, `autostart = true`, `abi = "linux"`. `init`
  starts it with `--background` at login and stops it with a `Quit` (a hard 3 s grace) or `SIGTERM`; the shell
  gives every running resident app a tray item, so webd has one and no tray code. There is no window.
* **Program** `webd-app` (`webkit/port/webd/main.cpp`, linked with `libxui_webd_host.a`): about 75 MB, loaded whole
  into RAM. **The first thread runs the service**, not WebKit: `init` and the shell know an app by its first
  thread (a lifecycle `Watch` from another thread is `ESRCH`, as is a notification), so the registry name and
  the lifecycle line live there. WebKit's run loop runs on a thread `main.cpp` makes (8 MiB stack); the Rust
  half calls `webd_engine_*` (each only queues a task for the loop) and the engine calls `webd_ui_*` back on
  that thread, where the callbacks copy and queue (`xui_webd::events::EventQueue`).
* **No network for a light view.** `os.lazy.webd.light.v1` has no way to ask for one, and the engine refuses
  every load but `data:`, `blob:` and `about:blank` (`webd-abi-test network`). Since slice 3 the manifest gives
  the NetworkProcess the network, the resolver's files, the certificate bundle and the profiles' folder: a
  full view whose client holds a network scope uses them. A full view without one carries the light views'
  lock too (`webd-abi-test nonet`). The image needs no network stack for light views (`LAZYOS_WEBD=1` does not
  ask for `LAZYOS_NETD`); a full view that loads from the network needs one (`run_demo.py --net`).

## The real Messenger binding (`xui-app/webd`)

The portable service takes a `Fabric` (events out, buffers in) and a `Call` per request; this crate is its host:

| Piece | File | What it does |
|---|---|---|
| requests | `serve.rs` | `xui_app::server::Server::register("app.os.lazy.webd.light", ...)`, `poll`, `Service::handle`, `reply` (a oneway message's reply is dropped); the kernel-stamped `SenderId` becomes the `Origin` |
| events | `fabric.rs` | `Fabric::send` is a oneway `msg_send` on the channel the client moved in (`-EAGAIN` slow, `-EPIPE` gone); a slot is `buffer_map` of the client's buffer, checked against the size the kernel reports, unmapped and closed on drop; `LoadHtml`'s bytes are read and the handle closed |
| lifecycle | `serve.rs` | `resident::Lifecycle::watch()`: `Quit` ends the loop, `Reopen("/selftest")` runs the built-in client |
| engine | `xui-webd` `ffi` | `FfiEngine` over `webd_engine_*`; `webd_ui_*` feed one `EventQueue` |
| doorbell | `doorbell.rs` | an `eventfd` (below) |
| watchdog | `heartbeat.rs` | a task posted to WebKit's main thread every 2 s while a view exists; unanswered for 20 s means the engine is hung (below) |
| markers | `lines.rs` + `serve.rs` | the service's notes as `WEBD:*` lines |

### Wake-up

The service thread parks in **one** `wait_any` on three things: its registered endpoint (a request), `init`'s
lifecycle channel (`Quit`) and an `eventfd` descriptor (`WAIT_FD`), with a one-second deadline. A Messenger
handle belongs to the thread that made it, so the engine's thread cannot touch the first two; but descriptors
are shared (`CLONE_FILES`), and a `write` to an eventfd wakes the wait. So:

* the `EventQueue` rings the doorbell after every event the engine queues (any thread: a frame, a state, a link
  click, a lost view, an engine marker) and the SIGTERM handler rings it (a bare `write`, async-signal-safe);
* a pass is: clear the doorbell, answer every queued request (at most 256, so a client flooding input cannot
  starve the rest), `pump` the queue into events, look at the lifecycle line, run the watchdog, park. The clear
  comes **first**, so a ring that lands during the pass leaves the descriptor readable and the next park returns
  at once: no wake-up is lost, none is spent twice on an empty queue;
* the one-second deadline runs the `Ping`s that find a dead client (`EPIPE`) and the watchdog's probes.

Idle cost: see "Measured", below.

### Engine loss

A web process that ends is `webd_ui_view_lost` for each of its views (the engine's job; the service answers
`ViewLost` and `ENODEV` until `DestroyView`). The engine as a whole is lost when WebKit's main thread stops
turning, and the service notices without help from the engine: the watchdog posts a probe task with
`webd_engine_run_on_main` every 2 s while a view exists, and a probe unanswered for 20 s is a hang. A hung
loop cannot be asked to quit and cannot be restarted in place, so the service says `WEBD:HANG:FAIL`, calls
`Service::engine_lost()` (every view gets `ViewLost(Other)`), and the program exits with status 4; the clients'
channels close. **The C ABI did not grow** (`webd_abi.h` is unchanged): a callback for "the network process
ended" was considered and dropped, because WebKit restarts the network process on demand and an idle light
view does not depend on it.

## Markers

The Rust glue prints (channel `Webd`, `libs/tracefmt`, through `xui_app::trace`); the engine's own lines
arrive as `WEBD:NOTE:<line>`.

| Line | Meaning |
|---|---|
| `WEBD:UP:PASS` | the host is up (doorbell made, engine started) |
| `WEBD:SERVE:app.os.lazy.webd.light` | the name is registered |
| `WEBD:SERVE:app.os.lazy.webd.view` | the full interface and its two scopes are registered (`SERVE:FAIL` leaves the light views working) |
| `WEBD:WATCH:WARN:...` | no lifecycle line (started by hand, not by `init`): only `SIGTERM` ends it |
| `WEBD:VIEW:CREATE client=<label> view=<id>` | `CreateView` (`<label>` is `app:<system_name>` for webd's own label, `label#<id>` for another app (the credential gate names
a label only to its holder or to `CAP_SETUID`), `none` for an unlabelled caller) |
| `WEBD:VIEW:DESTROY view=<id>` | the view ended: `DestroyView`, the client died, or the service stopped (a `NOTE` says which) |
| `WEBD:LOST view=<id> reason=<refused\|crashed\|memory\|other\|unknown>` | the engine gave the view up |
| `WEBD:NOTE:<text>` | a refusal, a dropped frame (rate limited), or an engine marker |
| `WEBD:NOTE:full view=<id> <what>` | a full view's page did something the service decided about: `held`, `held refused`, `held expired`, `hand-off`, `hand-off refused`, `dialog dismissed`, `toast`, `toast refused` (rate limited) |
| `WEBD:HANG:FAIL:...` | the watchdog fired |
| `WEBD:QUIT:REQUESTED grace_ms=<n>` / `WEBD:QUIT:PASS` | `init` asked / the program ended cleanly with no engine process left (`QUIT:FAIL:status=.. engine processes left=..` otherwise) |
| `WEBD:SELFTEST:*` | the built-in client (below) |

## The built-in client (`/selftest`)

`rhai` cannot call `CreateView` (a script cannot send a channel or a buffer), and an image should be provable
with no other client, so webd carries one: launching the resident app again with the path `/selftest`
(`rhai -e 'sys::init::launch("os.lazy.webd", "/selftest", 0)'`; `init` answers a second launch of a resident app
with `Reopen(args)`, and `args` must be one absolute path) starts a thread that does what an app does, through
the real road: it resolves `app.os.lazy.webd.light` in its own handle table, moves an event channel in with
`CreateView`, shares two pixel buffers with `AttachSlot` and the page with `LoadHtml`, and waits for `Frame`s
whose middle pixel is the page's background colour. It loads a second page (the slot is reused), then `SIGKILL`s
the web process under the view and expects `ViewLost(Crashed)`, `ENODEV` from a call on the lost view, and a
fresh view that paints again. Each step is a `WEBD:SELFTEST:STEP` line; the verdict is `WEBD:SELFTEST:PASS` or
`:FAIL:<why>`. It runs under webd's own label, so it does not prove the manifest grants a *client* what it needs
(`webddemo` and `tools/webd/run.py` do), and it cannot see the network (`webd-abi-test network` does).

```bash
python tools/run_demo.py --webd                                      # build and boot it (then the command above in the Terminal)
python tools/screenshot/qemu_session.py --image target/lazyos.img --out shots/webd \
    --script tools/screenshot/examples/webd_selftest.json            # image: LAZYOS_DESKTOP=1 LAZYOS_WEBD=1 LAZYOS_UI_PROBE=1 LAZYOS_XUI_AUTOSTART=term
```

## Building and starting

```bash
python tools/webkit/deps.py && python tools/webkit/port.py          # Docker; once (LAZYOS_JSC_VOLUME names the work volume)
python tools/webkit/webd.py                                           # the service library on the host, the link in Docker -> target/xui/xui-webd.elf
python tools/xui/build.py                                             # packages it as os.lazy.webd
python tools/run_demo.py --desktop                                    # every desktop ships webd: built when missing, LAZYOS_WEBD=1 (1 GiB OS volume)
python tools/run_demo.py --webd                                       # webd required (a failed build stops the run), big-app profile
python tools/run_demo.py --webd-demo                                  # + the demo client os.lazy.webddemo (tools/webd/run.py)
python tools/run_demo.py --desktop --no-webd                          # leave it out (LAZYOS_WEBD=0): Docs and Mail say the engine is not running
```

**Every desktop image ships webd** (docs/webd-plan.md P3: Docs and Mail draw their pages with it). `LAZYOS_WEBD` is `1`
(required: the build fails when `target/xui/xui-webd.elf` is missing), `0` (left out) or unset: a desktop ships it
when that file exists and warns when it does not (`build_support/webd_embed.rs`, `Switch`). The switch embeds the
package, `WebProcess`, `NetworkProcess` and `fonts.conf`, and opens webd with every session. `LAZYOS_WEBDDEMO=1`
adds the demo client (`CLIENT_PACKAGES`). In `tools/suites.toml` the flags `webd`, `no-webd` and `webd-demo` are
`test_only`: `base` is the desktop and so already has it. The launcher's Advanced tab has the controls.

## Measured

QEMU with WHPX, 4 GiB of guest RAM, 2026-10-21, an image with the self-test (`webd_lifecycle.json` for the
cost, `webd_selftest.json` for the times). Memory is the kernel's `frames_live` (4 KiB frames, the
`system/stats/memory` topic of `sysmond`); CPU is the task table's ticks (10 ms) and `PROC:EXIT cpu_ms`.

| | |
|---|---|
| **Idle CPU**, no view | the service thread (`webd.elf`) 0 ticks in 95 s; WebKit's four threads together 2 ticks (20 ms) since the start, all of it the rule-list compile at start; **about 0 %**. The one-second park deadline and the doorbell's descriptor wait cost less than a tick per minute |
| **Idle memory**, no view | 46,895 frames live running, 45,518 with webd stopped, 46,958 after starting it again: **1,377 to 1,440 frames, about 5.4 MiB** of anonymous memory (the program's own text is mapped from the page cache since #942 and is not in `frames_live`: the file's 75 MB are not counted here), and +6 frames (24 KiB) over 60 idle seconds |
| **One view held** (web process, network process, the view) | 54,237 frames: **+7,342 frames, about 28.7 MiB** over idle, and no CPU while it sits (every task 0 ticks) |
| **After `DestroyView`** | 49,315 frames: +2,414 (about 9.4 MiB) over idle stays: the network process and what the engine never frees of a view (docs/webd-plan.md, "What P1 found"); the web process is gone |
| **Cold first view** | `LoadHtml` to the first matching frame 1,031 ms (starts the web process; the network process and the rule list were ready) |
| **A view that is up** | `LoadHtml` to frame 32 ms; `Refresh` to frame 31, 33, 34 ms (min, median, max of 20): the Messenger hop out, the engine's paint (a 33 ms pacer) and the hop back. The hops are inside that pacer's noise; a hop alone needs a probe method that does no painting |
| **Whole lives** | `PROC:EXIT` of the web process after two pages and a kill: 30 to 40 ms of CPU |

## Full views (`os.lazy.webd.view.v1`, P2 slice 3)

What a browser needs, through webd: [`idl/webd.midl`](../idl/webd.midl), served with its two scopes as
`app.os.lazy.webd.view`. Every method the light interface has keeps its id and wire shape (the full one appends
fields: `State` has the address, the committed address, back/forward, progress and `secure`; `Frame` has
`whole`, `shift`, `rects` and the scroll offset), and `tools/webd/test_idl.py` holds the two files together.

**Scopes.** A view may reach what the interface its `CreateView` came through allows; the kernel checks the
parcel's interface against the caller's manifest before webd sees the call, as it does for the display's
pointer scope (`xui-app/crates/webd/src/fullconfig.rs`):

| Interface of the `CreateView` | Scheme policy | Profile | Hand-off, downloads, blocking, notifications |
|---|---|---|---|
| `os.lazy.webd.view.v1` (MEDIUM: "Show web pages that you give it") | `DataOnly` only; the view also carries the light views' network lock, so nothing of the page (an image, a fetch, a socket) reaches the network | `""` | no |
| `os.lazy.webd.net.v1` (HIGH: "Load web pages and pictures from the internet") | `Web`, `Origins`, `DataOnly` | `""` | no |
| `os.lazy.webd.browser.v1` (HIGH: "Browse the web with your browsing cookies and history, ...") | any | `""` or `browser` | yes (downloads, blocking and notifications need the `browser` profile; notifications need it on disk) |

Anything outside is `EACCES` at `CreateView`; every other call goes through `os.lazy.webd.view.v1`, which a client
lists with its scope. A light view and a full view live in one table: a call through the other interface finds
"no such view" (`ENOENT`).

**Profiles.** `""` is the client's own and stays **in memory**: webd may not name a client's label (the credential
gate names a label only to its holder) and label ids last one boot, so nothing would find a folder again.
`browser` is the one profile on disk, `$HOME/.apps/os.lazy.webd/profiles/browser` (cookies in
`cookies/cookie.jar.db`, cache, storage); `ephemeral` keeps its data in memory (a private window). The engine's
processes start in `profiles/` with `CURL_COOKIE_JAR_PATH=:memory:`, decided once at start
(`webd_engine_start_host`): each persistent profile's cookies are in its own file, which WebKit's curl session
opens per data store (`NetworkSessionCurl.cpp`), never in another's (`webd-abi-test profiles`, and the harness
below). Views of one client and one profile share a web process; at most **4 web processes per user** for full
views (a new (client, profile) pair past that is `EDQUOT`).

**Navigation policy** (`policy.rs`, decided on WebKit's thread from rules the service registered,
`events/rules.rs`): `about:`, `data:`, `blob:` always load; `http(s)` per the scheme policy (`Origins`: the listed
`scheme://host[:port]`, an address with a backslash, white space or a control character has no origin); `file:`
and `javascript:` never. Anything else is a hand-off: `Deny`; `Ask` (the engine holds the navigation, the client
gets `NavigationRequested`, no answer in 10 s is a no; a yes loads only an `http(s)` address); `Browser` (webd gives
it to `mimed`, only for the page itself, with a user gesture, within 3 s of real input the client forwarded). A
frame inside the page is allowed or refused, never asked about. The client's own `Load` and a `LoadHtml` base pass
the same policy. `Origins` limits where the page goes, not what it loads: its sub-resources come from anywhere.

**Questions and news.** `Dialog` waits for `DialogReply` (more than 3 in 10 s are dismissed without asking);
`NewWindowRequested` (nothing opens); downloads go to `$HOME/Downloads` under LazyWeb+'s naming rules
(`xui-app/crates/downloads`, `lazydownloads`: a `.part` file renamed when every byte arrived, never over another
file, 256 per session); a page's notifications are webd's toasts titled with the site's host, posted only for an
origin the view's profile allowed (`NotifyPermissionRequest` / `NotifyPermission`; WebKit keeps one permission
record for the whole context, so a decision of one profile reads as `Notification.permission` in another, and is
then refused at `show`); content blocking compiles the client's rule lists into the browser profile's store
(`BlockingCompile`), with a per-profile switch and exceptions (`SetBlocking`) and a one-shot `Reload(bypass)`. The
ad-block list's download (`listupdate`) stays with the client that owns the lists (LazyWeb+ today).

**For LazyWeb+ (P4).** `Recover(view, reload)` is the way out of a page whose process stopped answering
(`Responsive(false)`): the view's web process is ended, and with `reload` the page loads again in a fresh one;
without it the page stays stopped, which the engine reports as the page ended by request (`ViewLost`, reason
`Other`), and the client makes a new view; the engine refuses (`WEBPROCESS:RECOVER:SHARED`) while other views share
the process. `NotifyForget(view, origin)` forgets the profile's decision about `origin` (a view made with
`notifications`; `origin` is checked as in `NotifyPermission`): `Notification.permission` is `default` again and the
site's next request is asked again (`NotifyPermissionRequest`); WebKit's record is the context's, so it goes for
every profile.

**Notifications shown by the app** (`ViewConfig.notify_in_client`, with `notifications`; P4). The shell raises the
app that posted a toast when it is clicked, and an app cannot raise its own window, so a browser posts the page's
notifications itself. webd still keeps the profile's decisions and hands the app only what the profile allowed:
`NotifyShow(notification, origin, page_url, title, body, tag)` (each text cut at 16 KiB; an origin nobody allowed,
or more than 64 open at once in the view, is answered "closed" to the page and never reaches the app), then
`NotifyCancel` (the page closed it) and `NotifyGone` (its object is gone: the toast stays, a click reaches no
script). The app answers with `NotifyState(notification, Shown | Clicked | Closed)`, which runs the page's `show`,
`click` and `close`; `ESRCH` for an id that is over or not the view's, `EACCES` for a view without the flag. webd
posts no toast for such a view.

**Frames with damage** (`ViewConfig.deltas`). The paint plan is the window's (`xui-app/crates/damage`,
`lazydamage`, moved out of `xui-webkit` so both keep one set of rules), kept per view by the service
(`webd_paint_*`). A `Frame` with `whole = false` carries `shift` (move that area of the picture by `dx, dy`) and
`rects` (copy each from the slot, where it sits in a whole frame's layout) and the scroll offset the pixels were
painted at; a delta that cannot be delivered (no free slot, a full queue) ends the chain: the queue refuses deltas
until the engine paints a whole frame, which the view asks for when a slot is free
(`events/pictures.rs`, `service/deliver.rs`). Band slots 3 and 4 (`width * min(height, 1024) * 4` bytes) carry
`Band` events. `xui_webd_client::FullWebView` applies all of it to its picture.

**The proof** (`python tools/webd/full_run.py`, 2026-10-22, WHPX, 4 GiB): `os.lazy.webdbrowse` ("Web View Test",
shipped by `--webd-demo`) runs 17 steps against `tools/webd/full_server.py` (plain HTTP on 18081) and the judge
reads the client's markers, the server's request log, the OS volume and a screenshot: navigation, back and
forward; a rule list compiled and the blocked picture kept off the network until the bypassing reload; the
`browser` profile's cookie sent by two of its views and never by the client's own profile, whose own cookie never
reached the `browser` profile; a download whose file in `/home/user/Downloads` has the server's bytes; an alert
and a confirm answered; a `window.open` reported with `gesture=false`; a `mailto:` held and refused; a `CreateView`
with the network through `os.lazy.webd.view.v1` refused, and a view without the network scope that loaded HTML
with a remote picture which never reached the server; a delta frame with `dy=-40` after a wheel turn. No
`LABEL:DENY` of the client or webd; their children's only refusals are the documented optional ones.

| Measured (one run, `WEBDBROWSE:LATENCY`; the client polls every 8 ms, so 8 ms is "within one poll") | |
|---|---|
| `Load` of a page to its first frame (view up, page from the host) | 17, 32 and 25 ms (three runs) |
| wheel turn sent to the first frame after it (20 turns) | min / median / max 7 / 8 / 16, 8 / 8 / 8 and 8 / 8 / 8 ms |
| `Refresh` to frame (10) | 8 / 8 / 8, 8 / 8 / 8 and 6 / 8 / 8 ms |
| the engine's own wheel-to-damage (`w2d`, `INFO:PAINT`) | 2.0-2.1 ms average, 2.4-3.7 ms worst |

What the numbers do not say: they are a simple local page at 800x578; the light self-test's `Refresh` was 31-34 ms
because its pacer had just painted; and LazyWeb+'s scroll repro (Ars Technica, 2560x1440) was not run through webd
(no live site in the harness, and LazyWeb+ is not a webd client before P4). The hop costs at most one poll here.

**Not exercised in QEMU** (host-tested only, or in `webd-abi-test`): `Handoff.Browser` through `mimed`, a page's
notifications as toasts (the engine's half passes `webd-abi-test notify`), bands (patch 0030 painted none for a
240 or 40 px scroll in either test), `Origins`.

## Files

`$HOME/.apps/os.lazy.webd/rules/`: the engine's network-lock rule store, compiled at every start
(`LAZYOS_WEBD_RULES_DIR`, set by `webd_host_init`; the engine has no fallback: without the variable it prints
`FAIL:light:...` and refuses every light view, because a predictable folder in a shared place is not one a lock
may rest on). `$HOME/.apps/os.lazy.webd/profiles/`: where the engine's processes start; `profiles/browser/` is the
`browser` profile (cookies, cache, storage, the content-blocking store). A finished download is renamed in
`$HOME/Downloads`. A light view writes nothing else: no cache, history or cookies.

## Permissions

`xui-app/packages/webd/manifest.toml`, derived from runs of both sessions (`webd_selftest.json`,
`webd_lifecycle.json`) on an image built with `LAZYOS_LABEL_TRACE=1` (every refused call is a `LABEL:DENY`
line, AGENTS.md):

* **Interfaces: `os.lazy.webd.light.v1` only.** The kernel judges every outbound call *and oneway send* by its
  header (`authorize_parcel`, `kernel/src/ipc/syscalls.rs`), so the service's events on a client's channel are
  judged against webd's own label: the permission is the service's, not only the self-test's. A reply is not
  judged and the registration needs nothing (the name is in the app's own namespace, `app.os.lazy.webd.*`;
  a resolve of it is allowed by namespace). Known only by reading the kernel, not by a run: removing the
  entry also stops the self-test's first call, so no run shows the service's send refused. `webddemo`'s
  traced run (another branch) is the check from a client's side.
* **No `LABEL:DENY` for `app:os.lazy.webd` itself** in any run (start, a view, a killed web process, stop,
  start again, SIGTERM).
* **Files:** `write:$HOME/.apps/os.lazy.webd` (the rule store; no `/tmp` fallback was used) and the font reads of the
  page process.
* **The NetworkProcess holds nothing** (`network = []`, `files = []`): it starts, idles and ends, as a light view
  has no network. Its only refusals are the optional one LazyWeb+ documents (#836, `tools/webplus/denies.py`):
  a stat of `/etc/ssl/openssl.cnf`; the page process's are its stats of `/etc`, `/etc/localtime` and
  `/usr/share/zoneinfo` (the time zone probe). None of the fontconfig ones appeared.
* **Full views (P2 slice 3).** The manifest adds `os.lazy.webd.view.v1` (the full views' events go out with it),
  `os.lazy.mimed.v1` (a hand-off), `os.lazy.notify.v1` (the toasts; their clicks come back on the channel webd
  moved in, which needs nothing), `network = ["outbound"]` (a child label holds at most its app's), the resolver's
  files and the certificate bundle, and `write:$HOME/Downloads`; the NetworkProcess gets the network, those reads,
  the app's folder and `$HOME/Downloads`. The traced `full_run.py` (all of it but the hand-off and the toasts, which
  no QEMU run exercised: their grants are by reading) showed no `LABEL:DENY` of `app:os.lazy.webd` or of the client,
  and for the children only the optional ones above plus the page process's fontconfig probes
  (`tools/webd/judge.py`, `OPTIONAL_CHILD`). The client, `os.lazy.webdbrowse`, lists `os.lazy.webd.view.v1`,
  `os.lazy.webd.net.v1` and `os.lazy.webd.browser.v1` with the display and input.

## Limits, back-pressure and what grows (from the review of 2026-10-21)

* **Memory a client can cost.** Slots are charged the size the kernel reports for the mapped buffer, not the range
  the client declared, and the sums cannot wrap. Page-controlled text (a link, a copy, a title, an error, an engine
  marker) is clipped at 48 KiB when it is queued, so a script cannot grow webd by refusing navigations.
* **A client that stops reading** is put in a 250 ms back-off after a refused send: webd copies no frames for it and
  asks the engine for no whole frame until a send gets through. `ViewLost` is resent until delivered or the
  client's channel is gone. The client keeps slot releases and input release edges it could not send and resends
  them at each `poll`.
* **The console.** The engine's per-page markers (`LIGHT:LINK`, `LIGHT:REFUSED:*`, `CONFIG:IGNORED:*`, `INFO:*`) are
  flood-limited; the lifecycle markers (`PROCESS:*`, `FAIL:*`, `LIGHT:LOCK:READY`) never are.
* **The self-test** kills only a web process it can show is its own (two samples agree, it differs from the one
  before the view existed, its view is alone in webd's own group) and refuses with "cannot tell" otherwise: the
  ABI offers only the newest process id. A `webd_engine_view_pid(view)` would make it exact.
* **What still grows without bound.** Every view that ever was leaves its parked `WebPageProxy` (engine.cpp
  `destroyView`: a page proxy cannot be released on this port), and a client's first view of a group makes a data
  store and network session that stay too. An app that loops `CreateView`/`DestroyView` grows webd and the
  network process for the rest of the session. The fix is a pool of parked views reused per owner (P5); until
  then, the per-client view count limits only views alive at once.
