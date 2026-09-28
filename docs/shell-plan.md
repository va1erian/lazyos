# LazyShell — the LazyOS desktop shell (S5 on XUI)

**One line:** LazyShell is the S5 desktop: a session-scoped `os.lazy.*` client of
the `xuid` compositor, the Messenger services, and the `logind` session, painting
the desktop, taskbar, start menu, and bundled apps while the compositor keeps
ownership of window chrome, z-order, and focus.

Tracking: stage issue **#156**, tasks **#157-#162**.

This is the focused plan for stage **S5** of
[`platform-plan.md`](platform-plan.md), alongside [`xui-plan.md`](xui-plan.md)
(the S4 toolkit), [`messenger.md`](messenger.md) (the fabric it calls), and
[`security-model.md`](security-model.md) (session grants and policy). Baseline
detail per element is in the [architecture reference](architecture.md) and
[`architecture/display.md`](architecture/display.md).

---

## 1. Scope

**In scope:** the LazyShell process; desktop surface and wallpaper; taskbar and
start menu/launcher; Files, Settings, About, and the bundled core apps; theming
(Win95 flagship + the current dark palette); input, hotkeys, and focus routing
above the compositor; the session lifecycle that starts the desktop; and app
launch/supervision through `init` and `mimed`.

**Out of scope:** compositor internals and the display protocol below the shell
(S4, `xuid`/XUI); the signed-bundle install manager (S7); networking and remote
sessions (S6); GPU acceleration (S8).

## 2. Where we are today (honest baseline)

| Area | Today | Gap to target |
|---|---|---|
| Compositor | `xuid` binds the display grant, composites client surfaces with rectangle damage, and paints chrome; WM #143 adds drag, z-order, close/minimize, a taskbar, and `Tab` focus; #167 adds the desktop role, a shell subscriber, `GetWorkArea`/`GetTheme` and modifier-aware global hotkeys (Alt+Tab overlay, Ctrl+Esc/Super, Alt+F4) (`user/src/bin/xuid.rs`) | per-session theme, maximize/snap, a real shell consuming the events |
| Display protocol | `os.lazy.display.v1` methods 1-24: surfaces/buffers/commit, `Pointer*`/`Key*`, `WindowClose` (10), drag & drop (11-17), shell protocol (18-24: `ListSurfaces`, `GetWorkArea`, `Subscribe`, `GetTheme`, `SurfaceChanged`, `FocusChanged`, `StartMenu`) in `user/src/messenger.rs` (`display` module) | theme *write* path, resize, pointer-event payload tightening (button id, coordinate space) |
| XUI apps | `xui-app/` M0-M2 run the Counter on the display grant (#114); client mode runs xui apps in `xuid` windows with keyboard focus routing (#168, #151); `sysmon`/`fabricmon` viewers (#153) | LazyShell and the S5 apps themselves, timers/resize/DPI |
| Session | `logind` console login spawns the user's shell with kernel-stamped uid/gid/session; `SESSION_CAPS` is empty; `os.lazy.logind` exposes the session table (`user/src/bin/logind.rs`) | per-session compositor/clipboard/topic grants, graphical session bundle, session end reaping |
| Services | `messengerd` (#89, #92, #169 central broker), `init`/`logd`/`healthd` (#93), `keyd`/`accountsd`/`logind` (#101, #102), `clipboardd` (#115), `mimed` (#116), `sysmond` (#144); `init` app registry + `os.lazy.init.Launch` with session-owner check and supervision (#158) | `mimed.Open` still only publishes `system/events/open/<app>`; the task table (16 slots) is full under the services image |
| Shell/apps | native `sh` + BusyBox; the xuid fallback taskbar; `shellprobe` proves the shell protocol; no desktop, start menu, Files, Settings | all of S5's user-visible surface |
| Theme | xuid hard-codes its palette; the vendored XUI backend has a `set_theme` seam | one theme format, Win95 + dark, live selection |
| Evidence | `qemu_shot`/`qemu_session` + `pngstats.py`; the kernel test harness | scripted desktop sessions, golden captures, theme pairs |

## 3. Desktop model

- **Desktop surface.** LazyShell creates one full-screen background/desktop
  surface at session start: wallpaper, desktop icons (S5.3), and the root
  context menu. `xuid` composites it at the bottom of the z-order. With no shell
  attached, xuid paints its current background color as today, so the compositor
  remains usable alone (`xdemo`).
- **Window chrome stays in xuid.** Title bar, border, close/minimize buttons,
  drag-to-move, click-to-raise, z-order, and focus are compositor policy
  (#143) and remain there: one implementation, every client. LazyShell does not
  paint decorations, so a theme applied to chrome cannot drift per app.
- **Taskbar.** S5.0 moves the window list from xuid's interim bar into a
  shell-owned pinned surface at the bottom: start button, one entry per
  surface, clock, tray. `os.lazy.display.v1` gains append-only methods/events
  for a window list and focus changes (for example `ListSurfaces`,
  `SurfaceChanged`, `FocusChanged`) so the shell renders and switches windows
  without owning the surface table. The xuid built-in bar stays as the
  no-shell fallback.
- **Start menu.** A shell-owned popup surface (pinned above windows): Programs,
  Files, Settings, About, Log out, Restart. Its entries come from the app
  registry and MIME/open-with registrations (section 9), not a hard-coded menu.
- **Affordances.** Minimize/restore/close already flow through xuid events
  (`WindowClose`); maximize/snap and the Alt+Tab overlay land in S5.3 on the
  same chrome.

## 4. Session lifecycle

- **Login.** `logind` keeps the S3 pipeline: identify, authenticate through
  `accountsd`/`keyd` (Argon2id), mint a session id, and spawn the session
  programs with `spawn_as` so uid/gid/session are stamped before the first
  instruction.
- **Session programs.** S5 changes the session's default program from `sh` to
  the session bundle: `xuid` binds the display grant, then LazyShell attaches as
  an `os.lazy.display.v1` client. Per-session services stay one system process
  each where the service already scopes by session: `clipboardd` keeps per-session
  tables keyed by the kernel-stamped session id and refuses another session's
  tokens; `sysmond` remains a system service whose snapshots and
  `system/stats/*` topics the shell consumes as the session user. The
  compositor is per session (one `xuid` per login).
- **Grants.** `SESSION_CAPS` is empty today; S5.2 defines the session
  capability set (call the session's compositor and clipboard names, publish and
  subscribe under `session/<id>/*`, reach the home directory, read health/log
  views) and applies it at spawn. Default deny: the shell gets the session set
  and nothing more, and the kernel still checks every call.
- **Session end.** Log out (menu item, shell exit, or shell crash) tells `xuid`
  to stop, reaps the session's children, publishes the session's state on
  `system/events/login/session/<id>` (the shape `logind` already publishes),
  and returns to the login prompt. Ending a session must not affect another
  session or the system services.

## 5. LazyShell as an `os.lazy.*` client

The shell owns no device grants; it is one more policy-checked Messenger client.

| Need | Interface | State |
|---|---|---|
| Desktop/taskbar/menu surfaces, input events, close/raise | `os.lazy.display.v1` (`display` in `user/src/messenger.rs`) | exists; S5 adds desktop role, window-list/focus events, hotkeys |
| Copy/paste | `os.lazy.clipboard` + `os.lazy.clipboard.write.v1`/`.read.v1` | landed (#115) |
| Open with / resolve an app | `os.lazy.mimed` (`Guess`, `Lookup`, `Verbs`, `Open`, `Register`) | landed (#116); `Open` publishes `system/events/open/<app>` |
| Launch/supervise apps | `os.lazy.init` (new `Launch`) | no launch method yet |
| Session/account info | `os.lazy.logind`, `os.lazy.accountsd` | queries exist |
| Health, log, audit panels | `os.lazy.healthd`, `os.lazy.logd`, `os.lazy.audit.v1`, `system/health/*` | health topics and `logd` queries exist; `os.lazy.audit.v1` is spec |
| Task Manager numbers | `os.lazy.sysmond`, `system/stats/{memory,tasks}` | landed (#144) |
| Discovery/introspection | `os.lazy.messenger.registry`, `os.lazy.messenger.topics` | S1/S2 |
| File content for Files/Editor | native `read_file` plus the Linux ABI fd layer (`getdents64`) today; an `os.lazy.fs` reader/writer service is the target | no fs service yet |

## 6. Apps

- **Files (S5.1).** Home/volume browser: list, navigate, open (through `mimed`),
  reveal, copy/paste (through `clipboardd`), and a `session/<id>/selection`
  publication, the shape sketched in `messenger.md` section 19. Reads use the
  existing VFS paths (native `read_file` for native programs, `getdents64` for
  Linux-ABI `std` apps); when an `os.lazy.fs` reader lands, Files switches
  backend without a UI change.
- **Settings (S5.2).** Users and session (name, uid, home, session id, active
  grants), theme, display geometry, clipboard history policy, app permission
  view (the friendly/security model), and About (version, build, memory/task
  snapshot from `sysmond`).
- **Core apps (S5.3).** Editor (open-with target for `text/*`), Terminal
  (spawns `sh`/BusyBox under the session credentials), Paint (xui-skia canvas),
  Task Manager (`sysmond` snapshots plus the `messengerctl` views), Help (a docs
  viewer). Each registers open-with verbs in `mimed` and appears in the start
  menu through that registry.

## 7. Theming

- **One theme format, two consumers.** A theme is a palette plus metrics:
  window/client colors, active/inactive title bar, button/bevel style, border
  widths, font size. `xuid` consumes the chrome part; XUI apps consume the
  widget part; both receive the same per-session theme, so chrome and widgets
  cannot disagree.
- **Flagship Win95 theme:** gray 3D bevels, navy active title bar, teal desktop,
  square pixel metrics, the classic start button. The vendored XUI backend
  already has the `set_theme` seam (`xui-app/src/backend.rs`), and xuid's
  palette constants become theme fields.
- **Existing dark theme** (today's xuid palette) ships as the second built-in.
- **Selection:** a per-user preference read at session start and delivered to
  `xuid` through the display protocol, changed live from Settings in S5.2;
  applications repaint from the same source.

## 8. Input, keyboard routing, and focus

- The low path is unchanged: PS/2 IRQs feed the kernel display queue, drained
  only by the bound compositor. S5 never opens a second input path.
- `xuid` routes pointer and key events to the focused surface's event endpoint
  and owns focus policy: click-to-focus/raise and `Tab` cycling exist; S5.0 adds
  modifier-aware global hotkeys (Alt+Tab, `Ctrl+Esc`/Super for the start menu,
  Alt+F4, Escape to cancel a drag). The shell registers shell-local shortcuts
  through the same append-only grab call, so no client steals keys silently.
- Keyboard focus inside an app is the toolkit's job; the S5.0 work closes the
  XUI focus gap (a key with no pointer hit target reaches no widget, noted in
  #151) by routing to the focused widget. Menus and dialogs own focus within
  the shell.

## 9. Launch and supervision

- **File to app.** Files calls `os.lazy.mimed.Open(path, verb)`; `mimed` guesses
  the type, resolves the open-with registration, and publishes
  `system/events/open/<app>` (the current behavior).
- **App launch.** S5 adds the missing `init` path: `os.lazy.init.Launch(app_id,
  args, session)` (session owner only) spawns the app ELF as a session child
  with the session's stamped credentials and a per-app profile, and supervises
  it with the existing manifest machinery: restart policy, crash backoff
  (5 rapid restarts), `system/health/<name>`, and `system/events/service/<name>`.
- **Consumers.** Start menu, desktop icons, taskbar Run, and `mimed` launch
  requests all go through those two calls; `lazyosctl`/`messengerctl` keep the
  CLI view of the same state. Editor-like long-running apps pair with
  `OnFailure`; one-shot viewers with `Once`.

## 10. Stages

Each stage ends with evidence in CI; sizes are rough (S/M/L).

### S5.0 — Shell bring-up (M)
**Goal:** a user logs in and sees a desktop with a taskbar, and apps launch.
**Deliverables:** LazyShell process and build hook (a session bundle replacing
the `sh` spawn); desktop/background surface plus the xuid desktop role;
shell-owned taskbar (window list, start button, clock) with the append-only
`os.lazy.display.v1` window-list/focus additions; start menu/launcher fed by the
app registry; `os.lazy.init` `Launch` with the session-owner check; Alt+Tab and
global hotkeys in xuid; `logind` session starting `xuid` + LazyShell.
**Depends on:** S3 (#97); S4 (#112: #113, #114, #143; #115/#116 for clipboard
and open-with).
**Evidence:** a scripted headless session that logs in, captures the desktop and
taskbar, clicks Start, launches an app, and captures both windows
(`SHELL:DESKTOP:PASS`, `SHELL:LAUNCH:PASS`) with `pngstats.py`; kernel
correctness + soak tests only if the display-grant surface changes (new
`display.rs` ops in `kernel/src/tests/display_suite/`).
**Status (2026-09-28):** the protocol half landed: shell protocol + desktop
role + hotkeys (#167, PR #170), `init` `Launch` and app registry (#158, PR
#171), xui client mode and focus routing (#168, PR #172), each with an evidence
client (`shellprobe`, `apps_demo.json`, `xui_client.json`). Not started: the
LazyShell process (#157), the `logind` session bundle, and the CodeRabbit
follow-ups #175/#177/#178.

### S5.1 — Files and start menu (M)
**Goal:** browse and open files from the GUI.
**Deliverables:** the Files app (list, navigate, open, reveal, copy/paste,
selection topic); start menu Programs/Files entries wired to the app registry
and `mimed`; open-with round trip from Files to the registered app.
**Depends on:** S5.0; VFS (#98/#99); `clipboardd` (#115); `mimed` (#116).
**Evidence:** a scripted session opens Files, navigates `/home/<user>`, opens a
text file in the registered editor, copies a file, and pastes it
(`FILES:*` markers) with screenshots; any new kernel file path gets the AGENTS.md
test pair; the ABI bench stays green.

### S5.2 — Session, Settings, and themes (M)
**Goal:** the session is real and configurable, and themes are live.
**Deliverables:** the session capability set applied at spawn (compositor,
clipboard, session topics, home); per-session chrome theme in xuid; one theme
format shared by xuid and XUI; the Win95 flagship and dark themes; Settings
(users/session/theme/display/clipboard policy/app permissions) and About; log
out and coexistence of a console session with the graphical one.
**Depends on:** S5.0-S5.1; accounts/`logind`/`keyd` (#101/#102); the session
rules in [`security-model.md`](security-model.md).
**Evidence:** a scripted login asserts the session starts with exactly the
defined grants; a theme-switch screenshot pair (`THEME:*` markers); logout
returns to the prompt and kills the session's tasks (serial plus
`messengerctl sessions`); kernel correctness + soak tests for any credential or
session-check path touched.

### S5.3 — Polish: icons, notifications, multi-window (M)
**Goal:** the desktop feels finished.
**Deliverables:** desktop icons and an icon registry (16/32 px classic look);
tray and notifications via `os.lazy.notify` (the spec shape in
`messenger.md` section 11); window affordances (maximize/snap,
minimize-to-taskbar consistency, the Alt+Tab overlay, window menu); Editor,
Terminal, Paint, Task Manager, and Help registered open-with; keyboard
navigation/accessibility basics (focus order, scaling).
**Depends on:** S5.0-S5.2; WM (#143); drag & drop (#145) for desktop/menu DnD.
**Evidence:** one scripted session per affordance (Alt+Tab, snap,
minimize/restore, a notification, a drag from Files into the editor, the
platform-plan S5 acceptance) with screenshots and serial markers; repeated
window create/close soak for the WM path; kernel suite and ABI bench green.

## 11. Testing and evidence

Same policy as `platform-plan.md` section 6 and `AGENTS.md`:

| Layer | Method |
|---|---|
| Kernel | unit + soak tests under `kernel/src/tests/` for every kernel path S5 touches (display-grant ops, credential/session checks) |
| Shell/services | headless `qemu_session.py` scripts that log in and drive the GUI by input injection, with parseable `SHELL:*`/`FILES:*`/`THEME:*` serial markers |
| Visual | `qemu_shot`/`pngstats.py` on every capture; golden theme and desktop images; the existing screenshot workflow |
| Security | session-denial assertions: an app without the session grant cannot attach a surface or read the clipboard, and the denial is audited |
| Compatibility | the Linux ABI bench stays green throughout (hard CI gate) |

## 12. Risks

| Risk | Mitigation |
|---|---|
| Shell and xuid both grow taskbar/chrome and drift | one owner per concern: chrome/z-order/focus in xuid, window list/start/tray in the shell; the xuid bar is fallback-only |
| The shell needs surface events the protocol lacks | land the append-only display-protocol additions before the taskbar, and test both with and without a shell |
| No app launch path (`init` has no launch method) | resolved: `os.lazy.init.Launch` with the session-owner check landed (#158) |
| XUI keyboard focus gap (#151) | resolved: focus routing landed with client mode (#168); the scripted session types into an `Edit` |
| The 16-slot task table | the services image already fills it; LazyShell + `xuid` + apps need `MAX_TASKS` raised or a leaner manifest first |
| Heavy `std`/xui binaries and slow TCG boot | keep LazyShell lean (no app code linked in), measure boot in CI, use late capture timestamps |
| Session grants too broad or a session leak | default deny, an explicit tested session set, and a logout test that proves child reaping |
| Theme/DPI divergence | one theme format, fixed 96 DPI in S5; scaling is best-effort in S5.3 |

## 13. Open questions

- Per-session supervisor versus system `init` spawning session children: S5 uses
  `init` (spawn/wait/backoff/health already exist); revisit if sessions need
  independent restart budgets.
- One mediated `os.lazy.fs` service versus native directory-listing syscalls for
  Files and Editor; the UI is backend-agnostic either way.
- Does the taskbar live at the bottom of the framebuffer, or does the
  compositor report a work area to clients? A reported work area is cleaner and
  is the S5.0 preference.
- Desktop icon layout persistence: a per-user file first, the configuration
  registry when it exists ([`config-registry-plan.md`](config-registry-plan.md)).
- Notifications: a shell-owned toast path versus a separate `notifyd`; the spec
  allows either, S5 starts in the shell.

*See also:* [Platform plan](platform-plan.md) - [XUI plan](xui-plan.md) -
[Messenger spec](messenger.md) - [Security model](security-model.md) -
[Architecture reference](architecture.md).
