# Tray icons and resident apps (plan)

Status: **draft**. Covers the "tray" that `shell-plan.md` (S5.3) and
`platform-plan.md` promise for LazyShell's taskbar, and the app lifecycle
it needs: a *resident* app that keeps running with no window and shows itself
through a taskbar icon, a tooltip, a menu and, on a click, a small flyout.

Related: [`shell-plan.md`](shell-plan.md) (LazyShell, one owner per concern),
[`packages.md`](packages.md) (manifests, labels, consent, `autostart`, icons),
[`messenger.md`](messenger.md) (MIDL, topics, the `os.lazy.notify` sketch),
[`security-model.md`](security-model.md) §12 (the tray shield),
[`hidpi-plan.md`](hidpi-plan.md) (design pixels at 2x).

## 1. Goals and non-goals

**Goals**

- A background app (volume control, network status, a sync client, a
  LazyRAD monitor) has **one icon** in the taskbar, keeps it current
  (picture, tooltip, attention state, a short badge), and reacts to the user:
  primary click, secondary click, wheel, a **menu**, and a **flyout**
  anchored to the icon.
- An app can be **resident**: started at login (`autostart`), alive without
  any window, single-instance, opened from its icon, closed to its icon, and
  always quittable from the tray by the user.
- **A resident app always has an icon.** The shell shows one for every
  running resident app whether or not the app registered anything; the app
  only decides what it looks like and what it does.
- **The app handles its own lifecycle events.** "Open again" and "quit" reach
  the app on a channel it asks `init` for; `init` only falls back to killing
  an app that does not answer.
- Everything over **MIDL** interfaces and the package **label policy**.
- Works from Rust (`xui-app`), from native `user` programs, and from Rhai /
  LazyRAD forms ("Rhai first for small apps").
- Survives a shell restart and an app crash without stale or lost icons.

**Non-goals (here)**

- Notifications/toasts: `os.lazy.notify` stays its own plan (S5.3). This plan
  only leaves room for it (§10).
- System services drawing UI. A service with state worth showing (`audiod`,
  `netd`) gets a small session **applet** app that talks to it; services stay
  headless and session-less.
- Arbitrary app-drawn panels. The only app-drawn surface above windows is the
  flyout, and only right after a user click (§7.3).
- Several icons per app. One app, one item: an app with more to show puts it
  in its menu or flyout.

## 2. Prior art, and what we take

| System | Model | Taken | Left |
|---|---|---|---|
| Windows `Shell_NotifyIcon` | app hands HICON + tooltip; shell sends window messages; app draws its own popup menu | the simplicity, "click opens the app" habit, overflow chevron | app-drawn menus (inconsistent, focus bugs), no identity check |
| freedesktop StatusNotifierItem + dbusmenu | item is a service object; a watcher registry; the host renders a declarative menu | **declarative menu rendered by the host**, `Status` (Passive/Active/NeedsAttention), re-registration when the watcher restarts | the separate watcher daemon (we fold it into the shell, §4) |
| macOS `NSStatusItem` | menu or popover anchored to the item | **popover anchored to the item** = our flyout | — |

## 3. Overview

```
 resident app                LazyShell (os.lazy.shell.tray)        init                 xuid
 ────────────                ──────────────────────────────        ────                 ────
 Watch() ─────────────────────────────────────────────────────────▶ lifecycle channel
                             default item for each running  ◀───── session/{s}/apps/resident
                             resident app (package icon)            (retained)
 Set(item, menu) ──────────▶ app's item replaces the default
           ◀──────────────── Activate / Menu / Scroll / Ping
 (on Activate with a token)  AllowPopup(task, anchor) ───────────────────────────────▶ grant
 CreateSurface(role=Popup) ──────────────────────────────────────────────────────────▶ flyout
           ◀────────────────────────────────────────────────────── Reopen / Quit
                             "Open" row: init.Launch ─────────────▶ (already running)
                             "Quit" row: init.Stop ───────────────▶ Quit, then kill
```

- **The shell owns the tray** (rendering, order, overflow, menus, policy); the
  app owns its state and reacts to events. No new daemon.
- **One item per app**, keyed by the caller's kernel-stamped label (the
  `init` app id for unlabelled built-ins). There is nothing to name or count:
  `Set` creates or replaces the app's item, `Clear` returns it to the default.
- **Identity comes from the kernel**, never from the app: the tooltip and
  menu headers show the registry name `init.ListApps` gives that label.
- **Liveness is the event channel**: the shell `Ping`s it on the heartbeat
  (as `xuid` does to its clients); `EPIPE` drops the app's custom item. A
  resident app's default item stays until `init` reports it stopped.

## 4. Interfaces (MIDL)

New file `idl/tray.midl`, served by LazyShell under `os.lazy.shell.tray`
(generated stubs through `midlc`, `idl/manifest.json` updated, Rhai API
regenerated). Sketch; field ids and errors final at T0:

```idl
/// The taskbar tray (docs/tray-plan.md), served by LazyShell. One item per
/// app, keyed by the caller's kernel-stamped label. Callers must be in the
/// shell's login session and be uid 0 or the shell's uid; a labelled app
/// also needs this interface in its manifest (implied by `resident`).
interface os.lazy.shell.tray.v1 {
    /// Show the app's item, replacing its current one (custom or default);
    /// the parcel transfers the channel the shell sends the item's events on.
    method Set(item: Item = 1) -> () = 1
        transfers (events: Channel<os.lazy.shell.tray.events.v1>);
    /// Replace the given parts of the app's item (absent fields are kept).
    /// `ENOENT` before `Set`.
    method Update(icon: Option<Icon> = 1, tooltip: Option<String> = 2,
                  status: Option<U32> = 3, badge: Option<String> = 4,
                  menu: Option<Array<MenuItem>> = 5) -> () = 2;
    /// Drop the app's custom item: a running resident app goes back to its
    /// default item, any other app leaves the tray.
    method Clear() -> () = 3;

    struct Item { icon: Icon = 1, tooltip: String = 2, status: U32 = 3,
                  badge: Option<String> = 4, menu: Array<MenuItem> = 5,
                  activate: U32 = 6 /* Activation */ }
    /// At most one source; an empty or unusable `Icon` falls back to the
    /// app's package icon (§6.2), so an item never lacks a picture.
    /// `lucide` names an outline from the built-in set (kebab-case, e.g.
    /// `volume-2`), tinted with the bar's ink - the easy choice for an app
    /// without art; `mask` is an alpha-only image tinted the same way;
    /// `pixels` is RGBA at 1x and optionally 2x; `package` a PNG under the
    /// app's own `icons/`.
    struct Icon { lucide: Option<String> = 1, mask: Option<Image> = 2,
                  pixels: Option<Array<Image>> = 3, package: Option<String> = 4 }
    struct Image { width: U32 = 1, height: U32 = 2, data: Bytes = 3 }
    struct MenuItem { id: U32 = 1, parent: U32 = 2, label: String = 3,
                      kind: U32 = 4 /* MenuKind */, enabled: Bool = 5,
                      checked: Bool = 6, default: Bool = 7 }
    enum Status { Active, Passive, Attention }
    enum MenuKind { Normal, Check, Radio, Separator, Submenu }
    enum Activation { Event, Menu, DefaultItem }
}

/// What the shell sends the app about its item (oneway, on the `Set` channel).
interface os.lazy.shell.tray.events.v1 {
    /// Primary click; `anchor` is the icon in screen pixels, `popup` a
    /// one-shot token for a flyout (0 when flyouts are unavailable).
    method Activate(anchor: Rect = 1, popup: U64 = 2) -> () = 1 oneway;
    method SecondaryActivate(anchor: Rect = 1) -> () = 2 oneway;
    method MenuItem(id: U32 = 1, checked: Bool = 2) -> () = 3 oneway;
    method Scroll(delta: I32 = 1) -> () = 4 oneway;
    /// Liveness; nothing to answer.
    method Ping() -> () = 5 oneway;
    struct Rect { x: I32 = 1, y: I32 = 2, w: U32 = 3, h: U32 = 4 }
}
```

The app lifecycle channel is new in `init` (`idl/init_app.midl`), a separate
interface so an app gets it without being granted `os.lazy.init.v1`'s
`Launch`/`Stop`/`Shutdown`:

```idl
/// Served by `init` as `os.lazy.init.app`: a launched app's own line to
/// `init`. Only a task `init` launched (found by the kernel-stamped sender
/// in its table) may call; `ESRCH` otherwise.
interface os.lazy.init.app.v1 {
    /// Ask for this instance's lifecycle events; replaces an earlier channel.
    method Watch() -> () = 1
        transfers (events: Channel<os.lazy.init.app.events.v1>);
}

interface os.lazy.init.app.events.v1 {
    /// The app was launched again in this session (menu, desktop icon,
    /// `mimed`, the tray's Open row) while this instance runs: show yourself,
    /// open `args`.
    method Reopen(args: String = 1) -> () = 1 oneway;
    /// The user (or logout, or the package manager) asked the app to quit:
    /// save and exit within `grace_ms`, after which `init` kills it.
    method Quit(grace_ms: U32 = 1) -> () = 2 oneway;
}
```

Append-only additions elsewhere:

- `os.lazy.display.v1`: `Role::Popup` and `AllowPopup(task: U64, token: U64,
  x, y, w, h)` (shell-only) for flyouts (§7.3).
- `os.lazy.init.v1`: `AppInfo.resident`; the `Launch` reply gains
  `existing: Bool`.
- `os.lazy.shell.v1` `Status()` gains `tray: Array<TrayEntry>` (app, tooltip,
  status, custom or default, visible) so tests and scripts can read the tray.
- Topic `session/{session}/apps/resident` (retained, published by `init`):
  the session's running resident apps (app id, pid). Added to
  `idl/topics.midl` and `docs/topics-catalog.md`.

**Shell restart.** Items live in the shell, so a restarted shell starts
empty. Default items come back at once from the retained resident-apps
topic. For custom items the shell publishes a retained
`session/{session}/shell/tray` with a generation number once it serves the
interface; the client library subscribes and calls `Set` again on a new
generation (StatusNotifier's watcher-restart rule). Rejected alternative: a
separate `trayd` that outlives the shell. It adds a process, a second owner
of tray state and a new privilege boundary for a case (shell crash) the
re-registration already covers.

## 5. Resident apps

A manifest opts in under `[entry]`:

```toml
[entry]
binary = "bin/volume.elf"
autostart = true      # existing: start at login
resident = true       # new: may run with no window; single instance; always in the tray
```

- **Grants and consent.** `pkgstore::rules::compile` turns `resident = true`
  into the two interfaces a resident app needs, `os.lazy.shell.tray.v1` and
  `os.lazy.init.app.v1`, so the manifest's `interfaces` list stays about what
  the app does. The consent screen says "keeps running in the background and
  shows an icon in the taskbar". A non-resident app that wants a temporary
  icon (a long download) lists `os.lazy.shell.tray.v1` itself.
- **Single instance.** `init.Launch` of a running resident app in the same
  session starts nothing: it sends `Reopen(args)` on the instance's lifecycle
  channel and answers `existing = true` with its pid. An app that has not
  called `Watch` gets nothing; reacting is its job (§5.1).
- **Quit.** `init.Stop` (the tray's Quit row, logout, `pkgd` removing the
  package) sends `Quit(grace_ms)` first and kills the app only if it is still
  running after the grace period. The grace is a fixed **3 s** for every
  app, with no per-package override and no "this app is not responding"
  dialog: an app that has not exited by then is killed quietly
  (`INIT:APP:QUIT:TIMEOUT` on serial, a line in the service log). `Stop`'s
  existing callers keep their semantics, with the grace added. A resident app with durable
  state still serves `os.lazy.lifecycle.v1` for shutdown as today.
- **Restart policy** through `libs/svcpolicy`: a resident app that crashes
  after it ran past the start-up window is restarted with backoff (bounded
  like services); one that fails while starting keeps today's "stopped
  unexpectedly" notice and is not restarted. A `Quit`-requested exit is never
  a crash.
- **Publication.** `init` keeps `session/{session}/apps/resident` current on
  every start and exit of a resident app; that topic is what puts the default
  icon on the bar (§6.2).

### 5.1 What the app is responsible for

The mechanisms above deliver events; the app decides what they mean.
`xui_app::resident` (and the matching Rhai/LazyRAD API, T5) makes the usual
choices one line each, but the app owns them:

| Event | The app should |
|---|---|
| start | call `init.app.Watch`, then `tray.Set` with its icon, tooltip and menu |
| `Reopen(args)` | open or raise its main window, open `args` if any |
| `Activate` | its primary action (often the same as `Reopen`, or a flyout) |
| `MenuItem(id)` | run the action; send `Update` if a check or label changed |
| its last window closed | stay running (close to tray) or exit; its choice |
| `Quit(grace)` | save, exit before the grace runs out |
| tray generation changed | `Set` again (the library does this) |

**xui runtime.** `xui_app::launch` gains a resident entry point: the event
loop runs with zero windows, woken by the tray and lifecycle channels and
topic bells; windows open and close on demand. Whether
`xui_core::app::run_app` can idle with no surface, or needs a small
"no window" backend mode, is the first thing T3 checks.

## 6. Icons

### 6.1 Sources

In order of preference for a tray icon:

1. **`lucide`**: a name from the Lucide outlines xui already carries
   (`xui_core::Lucide`, the set `app-icons` draws package tiles with; about a
   hundred today). The shell draws it at 16 dp in the bar's ink, so it suits
   light and dark bars and HiDPI with no art from the app. The names come
   from the OS-wide named-icon library (§6.3); the tray is one of its
   users, not its owner.
2. **`mask`**: the app's own symbolic drawing, alpha only, tinted the same way.
3. **`pixels`**: full colour at 1x and 2x, drawn as given (a CPU graph, a
   coloured status dot).
4. **`package`**: a PNG from the app's own `icons/` directory.

### 6.2 The default icon

Every item resolves to a picture: an `Icon` that is empty, names an unknown
Lucide outline, or fails validation falls back, and so does a resident app
that never calls `Set`:

1. the package's `icons/app-16.png` (`app-32.png` at 2x), which every package
   must ship (`docs/packages.md`), read with the same size cap and PNG header
   check the desktop uses;
2. else (an unpackaged built-in, or an unreadable file) the Lucide
   `app-window` outline.

A **default item** (a resident app that has not called `Set`, or called
`Clear`) shows that icon, the app's registry name as tooltip, and a menu of
**Open** (`init.Launch`, which reaches the app as `Reopen`) and **Quit**; a
click is Open. So a resident app with no tray code at all is still visible,
reopenable and quittable, which makes "no invisible apps" a property of the
platform rather than of each app.

### 6.3 Named icons: a general service for xui apps (`lazyicons`)

Choosing an icon by name is useful far beyond the tray: toolbar buttons,
menu rows, LazyRAD forms, Rhai scripts, notifications, desktop shortcuts.
So the names are an **OS-wide contract of their own**, in a crate
independent of the tray and the shell: `xui-app/crates/named-icons` (crate
`lazyicons`), host-tested, usable by every xui app, LazyShell, `lrplay`
and the Rhai bindings.

- **The table.** One kebab-case name per `xui_core::Lucide` outline
  (`from_name`, `name`, `all`), with a test that it covers `Lucide::ALL`
  exactly. `xui_core::Lucide` has `ALL` but no name lookup; the mapping is
  kept on the OS side because the names are a stable API that packages and
  scripts depend on, while the toolkit's enum may be reshaped.
- **Stability.** Names are append-only: once shipped, a name is never
  removed or redrawn as something else (an outline replaced upstream keeps
  its old name as an alias). An unknown name is an error the caller sees
  (`None`, `EINVAL` over the wire), never a silent substitute; the *caller*
  decides its fallback (the tray uses §6.2).
- **Drawing.** `lazyicons::draw(canvas, name, rect, ink)` draws the outline
  at any size and scale with the stroke width Lucide expects, so an app, the
  shell and the player render a name identically; a small xui helper
  (`xui_app::icons::NamedIcon`) puts one in a layout.
- **Scripting.** Rhai gets `icons::names()` / `icons::exists(name)`, and
  LazyRAD form properties accept a name (`icon = "save"`) through the same
  table.
- **Discovery.** `docs/icons.md` lists every name with its outline, written
  by `cargo run -p lazyicons --example catalog` and kept current by a test,
  so app authors pick from the real set.
- **Growth.** The set grows by request (`volume-2`, `wifi`, `battery`,
  `refresh-cw`, ...): add the outline to xui, then append the name here.

## 7. The shell side

### 7.1 Model and layout (`lazyshell::tray`, host-tested)

- `Tray` holds at most one item per app, in display order: first appearance,
  then the order the user set (`user/<uid>/tray/order`, and
  `user/<uid>/tray/hidden/<app>` in `confd`; Settings writes them, §9 T5).
- Layout: the tray sits between the window entries and the clock.
  `taskbar::entry_rects(count, screen_w, right_reserved)` takes the clock
  plus the tray width; tray cells are 24 dp with 16 dp icons (32 px at 2x).
  At most `TRAY_VISIBLE` (6) cells, then an overflow chevron opening a panel
  with the rest; `Passive` items go to the overflow first.
- Limits (bounding one item's memory and paint cost, generous enough not to
  block an app): 64 items per session, images at most 64 x 64, tooltip 256
  chars, 64 menu rows, menu depth 2, badge 3 chars. Updates are coalesced to
  the shell's frame tick (at most about 10 repaints/s per item), so an app
  animating its icon cannot load the shell.

### 7.2 Painting and input (`xui-app/src/shell/tray/`)

- Split by responsibility (each file under 500 lines): `service.rs` (decode,
  authorize, apply), `resident.rs` (the resident-apps topic, default items),
  `icon.rs` (source resolution and fallback, Lucide names through
  `lazyicons`), `paint.rs`,
  `menu.rs` (the declarative menu as a shell `Panel`, reusing the start
  menu's look and keyboard navigation), `tooltip.rs`, `overflow.rs`,
  `liveness.rs`.
- Input: left click per the item's `activate` (`Event` -> `Activate`,
  `Menu` -> open the menu, `DefaultItem` -> the default row); right click
  opens the menu (or `SecondaryActivate` for an item without one); wheel ->
  `Scroll`; hover 500 ms -> tooltip panel headed by the verified app name.
- Every menu ends with a shell-added **Quit <App>** row (`init.Stop`), which
  the app cannot remove or relabel.
- Probe: under `LAZYOS_UI_PROBE=1` each cell prints a `UI:WIDGET` line
  (`tray:<app>`), so sessions click by name.

### 7.3 Flyouts (anchored popups)

A flyout is an app-drawn surface (a volume slider, a network list) next to its
icon. To keep apps from opening chromeless overlays at will:

1. On a click the shell mints a random one-shot token and calls
   `display.AllowPopup(app_task, token, anchor)`; xuid keeps it for about 2 s.
2. The app creates a surface with `Role::Popup` and that token. xuid places it
   against the anchor (above the bar, clamped on screen), gives it keyboard
   focus (unlike a `Panel`), no chrome, no taskbar or Alt+Tab entry.
3. It is dismissed by a click outside it, Esc, focus loss, or a second click
   on the icon; xuid sends the app a `WindowClose`.

Without a valid token `Role::Popup` is refused (`EACCES`); a token works once,
for the task it was minted for.

## 8. Security

- **Who may call.** The tray: the sender's kernel-stamped session must equal
  the shell's, its uid 0 or the shell's (as `os.lazy.shell.v1`), and a
  labelled app needs the grant (implied by `resident`). The lifecycle
  channel: only a task `init` launched, for its own instance. `Reopen` args
  go only to the instance in the launching session, never to another user's
  copy of the same app.
- **Spoofing.** The tooltip header, menu header and Quit row carry the
  registry name for the caller's label, not app text; one item per label, so
  an app cannot fill the bar; a `package` icon resolves only inside the
  caller's own install directory; flyouts need a click-minted token; an item
  never takes focus or covers windows.
- **Untrusted input.** Every decoded field is validated (lengths, image
  `width * height * 4 == data.len()`, menu `parent` references, unique ids,
  Lucide names against `lazyicons`); the generated codec is covered by the
  existing conformance fuzzing, and `lazyshell::tray` gets a seeded fuzz test
  over random set/update/clear/resident-topic sequences.
- **Denials** print `SHELL:TRAY:DENY uid=<n> label=<l> why=<...>` and, under
  `LAZYOS_LABEL_TRACE=1`, the kernel's `LABEL:DENY` lines.
- **Security shield** (`security-model.md` §12) becomes a shell-owned item on
  the same model once grants are observable.

## 9. Stages

Each stage lands with its evidence; markers are `SHELL:TRAY:*` on the shell
side, `INIT:APP:*` in `init` and `TRAYDEMO:*` in the sample app.

### T0 - Interfaces and model (S)
`idl/tray.midl`, `idl/init_app.midl`, the display/init/shell additions and
the resident-apps topic (append-only), generated stubs and Rhai API;
`lazyshell::tray` (model, layout, limits, order, icon fallback) and the
taskbar layout reserving the tray. No visible change. The named-icon
library `lazyicons` (§6.3) lands alongside as its own change, since other
apps can use it before the tray exists.
**Evidence:** `cargo test` for `lazyshell` (layout at 1x/2x, overflow,
limits, fallback chain, fuzz); midlc conformance.

### T1 - Icons and activation (M)
Shell serves `os.lazy.shell.tray`; paints `lucide`/`mask`/`pixels`/`package`
icons with the fallback, badge, `Attention` pulse, tooltip; click/wheel
events; liveness via `Ping`; re-`Set` on shell restart. Client library
`xui_app::tray` (Rust) and a minimal `libs/trayclient` for native programs.
Sample app `os.lazy.traydemo` from `tools/xui/new_app.py`, gated by
`LAZYOS_TRAYDEMO=1` / `run_demo.py --traydemo` / a launcher checkbox
(AGENTS.md front-end rule).
**Evidence:** session `tools/screenshot/examples/tray.json`: icon appears
(`SHELL:TRAY:SET app=<id>`), click reaches the app
(`TRAYDEMO:ACTIVATE:PASS`), a Lucide and a pixels icon in a screenshot pair,
a bad icon shows the package icon, app killed -> `SHELL:TRAY:CLEAR` within
one heartbeat, shell killed -> `SHELL:TRAY:RESTORED n=1`; `pngstats`
checks; no `LABEL:DENY` under the trace.

### T2 - Menus (S)
Declarative menus rendered by the shell (checks, radios, separators, one
submenu level, keyboard navigation), the unremovable Quit row (stopping
through `init.Stop` as it is until T3 adds `Quit`).
**Evidence:** `tray_menu.json`: open by right click, toggle a check
(`TRAYDEMO:MENU:<id>:<checked>`), Quit stops the app, item gone; light and
dark theme screenshots.

### T3 - Resident apps (M)
`entry.resident` in `lazypkg` (parser, consent text, implied grants in
`pkgstore::rules::compile`, tests); `init`: `os.lazy.init.app.v1`
(`Watch`, `Reopen`, `Quit` with grace), single-instance `Launch`, the
resident-apps topic, restart policy in `svcpolicy` (host-tested); default
items in the shell; `xui_app::resident` with the windowless event loop and
close-to-tray. First real applets as core packages: **Volume** (`audiod`,
shipped with sound images) and **Network** status (`netd`, with `--net`
images), autostart + resident, both on Lucide icons.
**Evidence:** `cargo test -p lazypkg -p pkgstore -p svcpolicy`; a session
with a resident app that never calls `Set` (default item, Open, Quit); the
demo closes its last window, stays alive and reopens from the icon
(`TRAYDEMO:REOPEN:PASS`); launching it from the menu again gives
`existing=true` and a `Reopen`; Quit gives `TRAYDEMO:QUIT:PASS` and a clean
exit, an app ignoring `Quit` is killed after the grace
(`INIT:APP:QUIT:TIMEOUT`); a crash after start-up is restarted, one at
start-up shows the notice; `tools/shutdown/run.py` still green; Volume's
wheel changes the level and `tools/sound/run.py` hears it.

### T4 - Flyouts (M)
`Role::Popup` and `AllowPopup` in xuid; the flyout API in `xui_app::tray`;
Volume's slider flyout.
**Evidence:** session opening, using and dismissing the flyout (outside
click, Esc, second click); a popup without a token is refused
(`XUID:POPUP:DENY`); a replayed token is refused.

### T5 - Overflow, settings, Rhai (M)
Overflow panel; Settings -> Taskbar pane (per-app show/hide/order, persisted
under `user/<uid>/tray/*`); Rhai `sys::tray` and lifecycle bindings and a
resident mode for `lrplay` (a form with no window, events from the tray and
lifecycle channels), with a LazyRAD sample packaged as an `.lzp`.
**Evidence:** session with eight items (six shown, two in overflow), a
hidden item stays hidden after reboot; `python tools/rhai/run.py --lazyrad`
variant with the Rhai tray sample.

No kernel change is planned (everything is userspace over existing
Messenger, policy and display paths). If one becomes necessary, it ships with
kernel correctness and soak tests per AGENTS.md.

## 10. Fit with notifications

`os.lazy.notify` (S5.3) stays separate but shares identity rules: a
notification is attributed by label the same way, and since an app has one
item, a notification of an app with an item can set its `Attention` state or
open its flyout when clicked.

## 11. Decisions and open questions

Decided:

1. **The shell owns the tray**, no `trayd` (§4); revisit only if
   re-registration after a shell restart proves unreliable.
2. **A resident app always has an icon** (§6.2): the default item from the
   package icon, else Lucide `app-window`; apps without art pick a Lucide
   outline by name.
3. **Reopen and quit are the app's job** over `os.lazy.init.app.v1` (§5.1);
   `init` only delivers them and kills after the grace.
4. **One item per app** (§3): no keys, `Set` replaces, `Clear` reverts.
5. **Named icons are a general OS service for xui apps**, not part of the
   tray: the `lazyicons` crate (§6.3), with stable append-only names, a
   shared drawing routine and Rhai/LazyRAD access; the tray is one client.
6. **The quit grace is a fixed 3 s** (§5): no per-package override, no
   "not responding" dialog; a late app is killed quietly and logged.

Left as an option:

1. **Session-less callers.** No system service needs a tray item today, so
   the tray accepts callers in the shell's session only, and services show
   state through a session applet. If one ever does, the natural extension
   is a system-wide item a uid-0 service sets for every session (the shell
   subscribing to a `system/tray/<service>` retained topic), without
   changing `os.lazy.shell.tray.v1`.
