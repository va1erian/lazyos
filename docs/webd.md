# webd

`webd` is the per-session daemon that owns WebKit and gives other apps **light views**: an app hands it a page
of HTML and gets the page's pixels back in shared memory, plus a few page events. Plan, security model and the
phases: [`webd-plan.md`](webd-plan.md). This file is the as-built description of the program (P2 slice 2);
the interface is [`idl/webd_light.midl`](../idl/webd_light.midl) (served as `app.os.lazy.webd.light`), the
portable service is `xui-app/crates/webd` (`xui-webd`), the engine half is `webkit/port/webd/abi.cpp`
([`webkit/README.md`](../webkit/README.md), "webd's engine half").

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
                                              app:os.lazy.webd/webprocess (one per client app, fonts only)
                                              app:os.lazy.webd/networkprocess (idle: a light view has no network)
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
* **No network.** `os.lazy.webd.light.v1` has no way to ask for one, the engine refuses every load but
  `data:`, `blob:` and `about:blank` (`webd-abi-test network`), and the manifest gives neither the program nor
  the NetworkProcess a socket, a resolver or a certificate file. The image needs no network stack
  (`LAZYOS_WEBD=1` does not ask for `LAZYOS_NETD`).

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
| `WEBD:WATCH:WARN:...` | no lifecycle line (started by hand, not by `init`): only `SIGTERM` ends it |
| `WEBD:VIEW:CREATE client=<label> view=<id>` | `CreateView` (`<label>` is `app:<system_name>`, `none` for an unlabelled caller) |
| `WEBD:VIEW:DESTROY view=<id>` | the view ended: `DestroyView`, the client died, or the service stopped (a `NOTE` says which) |
| `WEBD:LOST view=<id> reason=<refused\|crashed\|memory\|other\|unknown>` | the engine gave the view up |
| `WEBD:NOTE:<text>` | a refusal, a dropped frame (rate limited), or an engine marker |
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
python tools/run_demo.py --webd                                       # LAZYOS_WEBD=1: desktop, big-app profile, 1 GiB OS volume
python tools/run_demo.py --webd --dry-run                             # the flags, build steps and environment
```

`LAZYOS_WEBD=1` embeds the package (and the demo client's, when `xui-app/packages/webddemo` exists:
`build_support/webd_embed.rs` `CLIENT_PACKAGES`), `WebProcess`, `NetworkProcess` and `fonts.conf`, and opens webd with
every session. The launcher's Advanced tab has the same switch. Not in `--suite full` yet (`tools/suites.toml`,
`test_only`): until Mail and Docs use it (docs/webd-plan.md P3) it serves only its demo.

## Files

`$HOME/.apps/os.lazy.webd/rules/`: the engine's network-lock rule store, compiled at every start
(`LAZYOS_WEBD_RULES_DIR`, set by `webd_host_init`; the engine falls back to `/tmp/webd-rules-<pid>` without
`$HOME`). Nothing else is written: no cache, history or cookies.

## Permissions

Derived from runs under `LAZYOS_LABEL_TRACE=1` (`LABEL:DENY` lines; see "Permissions derived" in the P2 slice 2
section of [`webd-plan.md`](webd-plan.md)).
