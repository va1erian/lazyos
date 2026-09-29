# Display, input & mux

**What it is.** Rendering and input: the kernel text console, the two-window
multiplexer (kernel task), the userspace display grant, and the committed
compositor demo. Target toolkit design: [xui-plan.md](../xui-plan.md).

**Key files**

| Path | Role |
|---|---|
| `kernel/src/console.rs` | Framebuffer text console (anti-aliased glyphs) |
| `kernel/src/gfx.rs`, `surface.rs` | Framebuffer access; `RgbaBuffer`/`Surface` double buffering |
| `kernel/src/text.rs`, `font.rs`, `font-atlas/` | Positioned text draws; build-time glyph atlas |
| `kernel/src/cursor.rs` | Mouse cursor sprite overlay |
| `kernel/src/input/{keyboard,mouse}.rs` | PS/2 drivers (IRQ1/IRQ12) |
| `kernel/src/mux.rs` | Terminal multiplexer: paints task windows, Tab focus |
| `kernel/src/display.rs`, `display/buffers.rs` | Display device grant, syscall 12, input event queue; shared-buffer ops 4-6 |
| `user/src/bin/xuid.rs`, `xdemo.rs` | Compositor and demo app (issue #113) |
| `user/src/bin/dragdemo.rs` | Drag & drop demo pair (issue #145) |
| `user/src/messenger/` (`display` module) | `os.lazy.display.v1` client/server helpers |
| `xui-app/`, `tools/xui/build.py` | Ordinary xui app on the display grant (issue #114) |

**Kernel mux** (`mux.rs`)

- Runs as the kernel task (`task::KERNEL_TASK`, `Interactive` class). Renders up
  to two task windows side by side with titles; the focused window gets a green
  border and `*`. Repaints only when `NEEDS_REDRAW` is set (output, focus, task
  death), plus incremental mouse-cursor blits; sleeps `IDLE_TICKS = 2`.
- `Tab` cycles focus in `task::on_key`; Ctrl-C becomes `SIGINT` for the focused
  process group. While `display::bound()`, the mux stops painting and resumes
  with a full repaint when the compositor exits without unbinding.

**Display grant** (`display.rs`, issue #113; syscall 12)

| Op | Name | ABI |
|---|---|---|
| 0 | `bind` | out: `[width, height, stride, bpp, buffer, va, size]` |
| 1 | `unbind` | - |
| 2 | `input_poll` | in: buffer + capacity; out: event count |
| 3 | `present` | packed damage `x \| y<<16 \| w<<32 \| h<<48` |
| 4 | `create_buffer` | in: size; out: `[handle, va, size]` |
| 5 | `map_buffer` | in: handle; out: `va` |
| 6 | `close_buffer` | in: handle; unmaps and drops the reference (`-EBADF` if not held, `-EBUSY` for the bound compositor's screen buffer) |

- One owner at a time; the kernel task is refused; `bind` needs `CAP_SYS_ADMIN`
  (`-EPERM` otherwise: the owner sees every pixel and keystroke); every pointer
  argument is validated (`-EFAULT`), and `input_poll` only dequeues events that
  were actually copied out; an owner re-binding gets its geometry back. On bind the kernel creates a screen-sized RGBA8 shared buffer
  via `ipc::shared` and maps it in; `present` blits a clamped damage rectangle
  to the real framebuffer (`console::with_framebuffer`). Direct scanout is not
  used because the bootloader framebuffer frames live outside the allocator's
  usable regions (zero-copy scanout is an S8 follow-up).
- Input events (`Event`, 16 bytes) are pushed by keyboard/mouse IRQs into a
  256-entry queue and drained only by the owner; kinds are pointer move/down/up
  and key down/up with `key::*` codes for non-printables. `bound()` checks owner
  liveness (`task::live`), so no scheduler teardown hook is needed.

**Display protocol / XUI current state**

- `xuid` binds the grant and implements `os.lazy.display.v1` over Messenger:
  clients attach a shared surface buffer and an event endpoint, the compositor
  composites and routes input, `xdemo` is the smallest client.
- `xui-app/` (issue #114) runs an ordinary `xui-core` + `xui-canvas` app on
  LazyOS for milestones M0-M2. It is built by `tools/xui/build.py` for
  `x86_64-unknown-linux-musl` and embedded as `XAPP.ELF` when `LAZYOS_XUID=1`
  and `LAZYOS_XUI_APP=<path>` are set; the kernel then boots it *instead of*
  the `xuid` + `xdemo` session, because the app owns the display grant itself
  (`bind`/`present`/`input_poll`) and paints full-screen. Default
  `LAZYOS_XUID=1` is unchanged.
- **Client mode** (issue #168) adds `xui-client`
  (`xui-app/src/bin/client.rs`): with `LAZYOS_XUI_CLIENT=1` as well, the
  kernel boots `xuid` *and* the app, which never binds the grant.
  `LazyOSBackend::new_client` resolves `os.lazy.display.v1` over the raw
  syscall-5 shim, creates a surface, attaches a display shared buffer
  (`create_buffer`), commits per-node damage rectangles, and consumes
  `POINTER_*`, `KEY_*` and `WINDOW_CLOSE` events from its event endpoint. The
  WM (drag, minimize, taskbar, close) runs in `xuid` and works on the app
  window; the session is scripted in
  `tools/screenshot/examples/xui_client.json` and captured by the workflow.
  Two `xuid` protocol gaps are worked around in the client backend and worth
  tightening later: `PointerDown`/`PointerUp` carry surface-relative
  coordinates but no button id, and `PointerMove` carries screen-absolute
  coordinates (the client recovers the surface origin from the last press).
- **Keyboard focus routing** (issue #151) is mode-independent: a pointer press
  on a focus stop moves the backend focus, `SetFocus`/`KillFocus` reach the
  widgets, and `KeyDown`/`KeyUp`/`Char` target the focused node, not the node
  under the pointer. `Tab` cycles focus when the app owns the display;
  in client mode `xuid` reserves `Tab`, so `PageDown`/`PageUp` cycle instead
  (the kernel's PS/2 driver decodes neither F-keys nor a distinct Ctrl+Tab).
- Issue #153 adds the first windowed system-state viewers on that backend:
  `sysmon` renders the syscall-14 snapshot (frame/slab/heap gauges, uptime, the
  task table) and `fabricmon` renders the syscall-5 fabric (registry names with
  owners/interfaces, topics-broker counts, buffers/fences/handles, per-task
  usage). Each is one owner-drawn node with a one-second `ui` timer and `r`/`q`
  keys, prints `SYSMON:*`/`FABMON:*` serial markers, and is captured in
  `.github/workflows/xui.yml` as the display owner in turn (fabricmon over the
  `LAZYOS_SERVICES=1` session, so the registry and broker are live).
- Window management (issue #143) lives in `xuid`: the `surfaces` vector is the
  z-order (tail paints last), a title-bar press drags the window (clamped to the
  screen above the taskbar), the title bar carries close/minimize buttons, and a
  bottom taskbar lists live surfaces with the focused entry highlighted.
  Minimized surfaces are hidden and restored from the taskbar; `Tab` cycles
  focus skipping minimized ones. The close button sends the client a one-way
  `WindowClose` (method 10) event, which `xdemo` treats as "exit". Only
  `Commit` uses per-surface damage; WM layout changes repaint the full screen
  (a drag repaints the union of the old/new window rectangles).

**Drag & drop (issue #145)**

`os.lazy.display.v1` gains additive methods (11–17) that move a typed payload
between surfaces through the compositor, while `clipboardd` stays the data
broker. The wire fields reuse the clipboard's offer/token model, so the
compositor never sees payload bytes:

| # | Method | Direction | Fields |
|---|---|---|---|
| 11 | `DragStart` | app → compositor | `SURFACE`, `TOKEN`, `MIME` |
| 12 | `DragCancel` | app → compositor | `SURFACE` |
| 13 | `DragEnter` | compositor → app | `A`/`B` = surface-local x/y, `MIME` |
| 14 | `DragOver` | compositor → app | `A`/`B` = surface-local x/y |
| 15 | `DragLeave` | compositor → app | – |
| 16 | `Drop` | compositor → app | `A`/`B` = surface-local x/y, `TOKEN`, `MIME` |
| 17 | `DragEnded` | compositor → source | `A` = 1 dropped / 0 cancelled |

- The source offers the payload to `clipboardd` first (`Offer`/`write` scope)
  and calls `DragStart` with the returned token while a pointer button is held.
  The compositor only accepts it from the task that created the surface
  (`CreateSurface`'s sender), one drag at a time.
- While the drag is live `xuid` owns the pointer: it hit-tests the topmost
  surface under it (the source is never a target), sends
  `DragEnter`/`DragLeave`/`DragOver` to that surface, frames it in the drag
  accent colour, and draws a payload-label ghost at the cursor. The source
  receives no pointer moves between `DragStart` and `DragEnded`.
- Releasing over another surface sends `Drop` with the token; the target
  pastes through `clipboardd` with its own credentials, so the service's
  session scope still decides whether the transfer happens — a cross-session
  drop is refused (`-EACCES`) and audited like any other paste. Releasing over
  the source or the desktop, pressing `Escape`, calling `DragCancel`, or
  destroying the source/target surface sends `DragLeave` plus a cancelled
  `DragEnded(0)`.
- `dragdemo` (`DRAGDMO.ELF`) is the evidence pair. The kernel boots it in the
  `LAZYOS_XUID=1` path; with no manifest argument it is a launcher and starts a
  `source` and a `target` child (one clipboard session). It logs
  `DND:START:PASS`, `DND:DROP:PASS`, `DND:CANCEL:PASS` and `DND:DENIED:PASS`
  (the target re-tries its dropped token from another session and expects the
  clipboard's refusal).

**Shell protocol (issue #167, S5.0)**

LazyShell (S5) is an ordinary `os.lazy.display.v1` client, so `xuid` grows an
append-only set of methods and one-way events; unknown fields and methods are
ignored by older peers, and the no-shell sessions above are unchanged.

| # | Method | Direction | Fields |
|---|---|---|---|
| 18 | `ListSurfaces` | shell → compositor | reply: one row per surface — `SURFACE`, `TITLE`, `X`/`Y`/`W`/`H`, `MINIMIZED`, `FOCUSED`, `ROLE` (0 window, 1 desktop) |
| 19 | `GetWorkArea` | shell → compositor | reply: `X`/`Y`/`W`/`H` available to windows |
| 20 | `Subscribe` | shell → compositor | `SUBSCRIBER_ROLE` string + transferred event endpoint |
| 21 | `GetTheme` | shell → compositor | reply: `TITLE_BG_ACTIVE`, `TITLE_BG_INACTIVE`, `BORDER`, `TASKBAR`, `TEXT` as `0xRRGGBB` |
| 22 | `SurfaceChanged` | compositor → shell | `SURFACE`, `A` = created/destroyed/moved/minimized/restored/title, geometry + flags, `ROLE`, `TITLE` on create |
| 23 | `FocusChanged` | compositor → shell | `SURFACE` (0 = none) |
| 24 | `StartMenu` | compositor → shell | – (the Ctrl+Esc/Super hotkey fired) |

- **Desktop role.** `CreateSurface` gains a `ROLE` field (`0` window, the
  default when absent; `1` desktop). A desktop surface paints at the bottom of
  the z-order — above the background colour, below every window — with no
  chrome, no taskbar or Alt+Tab entry, and it never takes focus or hit-tests.
  Creating a new desktop replaces the previous one.
- **Authorization.** Claiming the `"shell"` role, creating a desktop surface
  and `ListSurfaces` are compositor-privileged: xuid reads the sender's
  kernel-stamped credentials and requires uid 0 or `CAP_SETUID`; anyone else
  gets `-EACCES` (a transferred handle is closed). A shell that dies (`EPIPE` on
  an event) is dropped, the fallback taskbar returns and the screen repaints;
  replacing a desktop or subscription closes the old endpoint.
- **Taskbar fallback.** The bottom taskbar stays xuid's no-shell fallback.
  `Subscribe` with role `"shell"` hides it and expands `GetWorkArea` to the
  whole screen; without a subscriber (or with any other role) the bar paints
  and `GetWorkArea` excludes its strip. Existing WM and drag & drop sessions
  run with no shell and are unaffected.
- **Global hotkeys** are modifier-aware and compositor-owned: the kernel
  forwards Shift/Ctrl/Alt/Super press/release plus F-keys to the bound
  compositor (`display::key` 0x108-0x10B, 0x113), and `xuid` consumes them.
  `Alt+Tab` opens a centered window-title overlay, repeated Tab cycles the
  selection, releasing Alt restores/raises/focuses the selection and emits
  `FocusChanged`; `Ctrl+Esc` and `Super` send `StartMenu`; `Alt+F4` sends
  `WindowClose` to the focused surface exactly like its `X` button; `Escape`
  still cancels a drag & drop.
- `shellprobe` (`SHELLPRB.ELF`) is the evidence client: it registers as the
  `"shell"` subscriber, creates a full-work-area desktop, reads back
  `ListSurfaces`/`GetWorkArea`/`GetTheme`, creates one window, and logs
  `SHELLPROBE:DESKTOP:PASS`, `SHELLPROBE:LIST:PASS`,
  `SHELLPROBE:FOCUS:PASS` (first `FocusChanged`), and
  `SHELLPROBE:HOTKEY:PASS` (first `StartMenu`). It also re-runs itself as an
  unprivileged child (`SHELLPRB.ELF denied`) that must be refused the shell
  role, a desktop and the list, logging `SHELLPROBE:DENIED:PASS`. It boots only with
  `LAZYOS_XUID=1` plus the `LAZYOS_SHELLPROBE=1` demo hook, so the default
  compositor sessions keep their window layout.

**Invariants.** Mux is always the fallback: no compositor state is required to
paint. The screen buffer handoff app-to-compositor is zero-copy (shared
buffers); only the final composite is a memcpy per damage rectangle. Input
events go only to the bound compositor; `push_event` drops the oldest event when
the queue is full and is IRQ-safe (leaf lock). The shell is a pure observer of
the surface table: the desktop role and `Subscribe` never hand the shell
compositor state, and a shell that exits leaves the fallback compositor usable.

**Status.** Working: demo mux, display grant, xuid + xdemo in headless captures
(`LAZYOS_XUID=1`), xuid window management (drag, z-order, buttons, taskbar,
focus cycling; `XUID:WM:PASS`), compositor-mediated drag & drop with a
clipboard-token transfer (`dragdemo`; `DND:*:PASS`), the shell protocol
(desktop role, surface list/work-area/theme read-backs, shell events, global
Alt+Tab/Ctrl+Esc/Alt+F4 hotkeys; `SHELLPROBE:*:PASS`), the xui app milestones
M0-M2 (`XUIAPP:*:PASS`), the sysmon/fabricmon viewers (`SYSMON:*`/`FABMON:*`
markers, screenshots in the `xui-app` workflow), compositor client mode
(`xui-client` inside a `xuid` window; `XUIAPP:CLIENT:PASS`, `XUIAPP:KEY:PASS`,
`XUIAPP:CLOSE:PASS`) and keyboard focus routing (click-focus, Tab /
PageDown cycling, keys to the focused widget). Open: zero-copy scanout,
tightening the pointer-event payloads noted above, userspace XUI toolkit,
multi-session compositors, drag targets that can refuse a drop before release.
