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
| `user/src/messenger/display/typeface.rs`, `user/build.rs` | `display::Face` (Droid Sans / Serif) proportional anti-aliased text for `xuid` chrome; atlases built from `assets/fonts/` |
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
  256-entry queue and drained only by the owner; kinds are pointer move/down/up,
  key down/up with `key::*` codes for non-printables, and the wheel
  (`POINTER_WHEEL`, `a` = notches, positive scrolls up). `bound()` checks owner
  liveness (`task::live`), so no scheduler teardown hook is needed.

**Display protocol / XUI current state**

- **The wire is MIDL** (issue #287): `os.lazy.display.v1` is defined once in
  `idl/display.midl` (methods 1-24 pinned with `= N`; app-to-compositor calls
  are request/reply methods, compositor-to-app events are `oneway`; the
  `Role`/`Change` enums, `SurfaceRow` and `Array<SurfaceRow>` for
  `ListSurfaces`). `xuid`, the `user` client library (`display::wire`), the
  demo apps and `xui-app` all use the generated `messenger-generated` stubs; no
  hand-written method or field ids remain. Field ids are positional, so the
  numbering changed from the pre-MIDL protocol (all peers ship together), the
  interface id is the generated hash of the name, and the structured error
  field is hand-written outside the generated range (id 15). Endpoint and
  buffer transfers stay in the parcel's `handles`/`buffers` vectors (the kernel
  moves those; the body has no `Handle`/`Buffer` fields).
- `xuid` binds the grant and implements `os.lazy.display.v1` over Messenger:
  clients attach a shared surface buffer and an event endpoint, the compositor
  composites and routes input, `xdemo` is the smallest client.
- `xui-app/` (issue #114) runs an ordinary `xui-core` + `xui-canvas` app on
  LazyOS for milestones M0-M2. It is built by `tools/xui/build.py` for
  `x86_64-unknown-linux-musl` and embedded as `/system/bin/xapp` when `LAZYOS_XUID=1`
  and `LAZYOS_XUI_APP=<path>` are set; the kernel then boots it *instead of*
  the `xuid` + `xdemo` session, because the app owns the display grant itself
  (`bind`/`present`/`input_poll`) and paints full-screen. Default
  `LAZYOS_XUID=1` is unchanged.
- **Client mode** (issue #168) adds `xui-client`
  (`xui-app/src/bin/client.rs`): with `LAZYOS_XUI_CLIENT=1` as well, the
  kernel boots `xuid` *and* the app, which never binds the grant.
  `LazyOSBackend::new_client` resolves `os.lazy.display.v1` over the raw
  syscall-5 shim, creates a surface, attaches two display shared buffers
  (`create_buffer`) as buffer slots, presents per-node damage through the
  pipelined `Present` (see below), and consumes `POINTER_*`, `KEY_*`,
  `WINDOW_CLOSE` and frame events from its event endpoint. The
  WM (drag, minimize, close) runs in `xuid` and works on the app
  window; the session is scripted in
  `tools/screenshot/examples/xui_client.json` and captured by the workflow.
  Several apps can share one `xuid` session (issues #215/#216): `sysmon`,
  `fabricmon` and `counter` call `LazyOSBackend::connect` (`xui-app/src/launch.rs`),
  which is client mode with `--client` and otherwise tries the grant and falls
  back to client mode when `xuid` holds it. `LAZYOS_XUI_APPS` embeds a list of
  apps (`/system/bin/{terminal,sysmon,fabricmon,counter,editor,files,paint}` + `/system/etc/xapps.lst`) and `init`'s app
  registry launches the `autostart` ones with `linux:PATH --client` (the kernel's
  `spawn` selects the Linux ABI from the `linux:` prefix,
  `kernel/src/process/spawn_line.rs`). The whole recipe is the single
  `LAZYOS_DESKTOP=1` switch (issue #217), which also drops the demo/evidence
  programs. The **Terminal** (`xui-term`) is a client
  that spawns BusyBox `sh` as a real child over a pipe pair and parses its
  output (CR/LF/BS and the CSI sequences its line editor emits) into a character
  grid (issue #254). There is no controlling tty yet, so it is a pipe-pair
  terminal rather than a kernel pty. Window placement (issue #250) tiles new
  windows in the first free grid cell of the work area (the screen minus the
  shell's taskbar, see `SetWorkArea`) and cascades with wraparound once it is
  full, so every window keeps at least its title bar visible; the LazyShell
  taskbar and Alt+Tab switch between them.
  The two early pointer-payload gaps are closed (issue #287): `PointerDown` and
  `PointerUp` carry the button id, and `PointerMove` is surface-relative like the
  presses (relative to the focused surface, negative or oversized while a
  press-and-drag leaves it), so the client backend needs no origin recovery.
- **Mouse wheel.** `kernel/src/input/mouse.rs` runs the IntelliMouse handshake
  at boot (sample rate 200, 100, 80, then *get id*; id 3 means a wheel), and
  from then on reads 4-byte packets whose last byte is a signed count, positive
  toward the user. The driver negates it, so the display event is **up-positive**
  like the protocol below, and queues one `POINTER_WHEEL` record per packet that
  rolled the wheel. A mouse that ignores the handshake keeps 3-byte packets and
  never produces the event. `xuid` forwards it as the one-way `PointerWheel(x,
  y, delta)` (method 31) to **what is under the pointer**, not the focused
  window: a shell panel, else the topmost window when the pointer is over its
  content (a title bar, a border or a drag swallows it; it never falls through
  to a window below), else the shell's desktop (`xuid/wheel.rs`,
  `XUID:WHEEL:PASS`). `xui-app` turns it into `Event::MouseWheel` for the widget under
  the pointer, one notch being `120` (Windows' `WHEEL_DELTA`, which xui's
  widgets and `xui-litehtml` expect). Horizontal wheels are not reported.
- **Pointer from `inputd`** ([../usb-hid-plan.md](../usb-hid-plan.md), P0-P2).
  Each PS/2 packet is also published on the raw input bus by
  `kernel/src/input/mouse_tap.rs` (`device::PS2_MOUSE`; screen-oriented
  `REL_MOTION`, `BUTTON` edges, an up-positive `SCROLL`), and `inputd` keeps
  the one cursor every pointing device moves (`inputmap::Pointer`). It reports
  `PointerEvent` (absolute position, button mask, wheel) on
  `os.lazy.input.shell.v1`, but only to a compositor whose `SetBounds` or
  `GetPointer` succeeded. `xuid` makes both calls when it attaches
  (`xuid/inputlink.rs`) and turns each event into its usual internal
  move / wheel / press / release (`xuid/pointer_feed.rs`, boot self-test
  `XUID:POINTER:PASS`), so hit-testing, drags and the `display.v1` events
  below are unchanged. While attached it drops the pointer records of the
  display stream described above and keeps only its keys; if `inputd` dies it
  releases the buttons it held, uses the display stream again, and takes the
  pointer back once `inputd` is restarted (`xuid: pointer from inputd` /
  `xuid: pointer back on the kernel stream`; session
  `tools/screenshot/examples/xuid_pointer_restart.json`). The added hop costs
  at most one `inputd` bus poll (2 ticks) over the direct path.
- **Keyboard focus routing** (issue #151) is mode-independent: a pointer press
  on a focus stop moves the backend focus, `SetFocus`/`KillFocus` reach the
  widgets, and `KeyDown`/`KeyUp`/`Char` target the focused node, not the node
  under the pointer. `Tab` cycles focus when the app owns the display;
  in client mode `xuid` used to reserve `Tab`, so `PageDown`/`PageUp` cycled
  instead; `Tab` now reaches the client (see *Key codes clients receive*), and
  the compositor takes only Alt+Tab, Ctrl+Tab (cycle windows) and an unfocused
  Tab.
- **Keys now have their own interface.** Windowed clients get keystrokes from
  `inputd` (`os.lazy.input.v1`, `idl/input.midl`, [../input-plan.md](../input-plan.md)):
  they `Open` a session for a surface they created and receive `KeyEvent`
  (physical HID `code`, `sym`, modifier bits, `Down`/`Up`/`Repeat`) and
  `TextInput` directly from `inputd`, only while focused. `xuid` is the shell
  client of `inputd` (`os.lazy.input.shell.v1`: it registers each surface's
  creator and reports focus) and stops carrying keystrokes for a surface once
  `inputd` reports a session for it (`SessionOpened`). The frozen
  `KeyDown`/`KeyUp` below are still synthesised, from the kernel's legacy
  stream, for surfaces without a session (native demo clients, images without
  `inputd`).
- **Key codes clients receive** (`KeyDown(key)` / `KeyUp(key)`, method 8/9 of
  `os.lazy.display.v1`). `key` is a `u32`: the **code** in the low 24 bits
  (`key & 0x00FF_FFFF`) plus **modifier bits** in bits 24-27, added by `xuid`
  from the modifiers it tracks. Modifier keys themselves are never forwarded.
  A client that ignores the modifier bits still sees the codes it always did
  (only Ctrl/Alt chords and Shift+non-printable differ). Held keys repeat as
  repeated `KeyDown` with no `KeyUp` (PS/2 typematic).

  | Code | Key |
  |---|---|
  | `0x20`-`0x7E`, `0xA0`-`0xFF` | printable character, layout and Shift applied (`'A'`, `'!'`, `'e'`-acute) |
  | 8 / 9 / 13 / 27 / 32 | Backspace / Tab / Enter (also keypad Enter) / Escape / Space |
  | `0x100` `0x101` `0x102` `0x103` | Left, Right, Up, Down |
  | `0x104` `0x105` | PageUp, PageDown |
  | `0x106` `0x107` | Home, End |
  | `0x10C` `0x10D` | Delete, Insert |
  | `0x110` + (n-1) | F1..F12 (`0x110`..`0x11B`; F4 is `0x113`) |
  | (`0x108`-`0x10B`) | Shift/Ctrl/Alt/Super: compositor-only, never sent to clients |

  | Bit | Mask | Meaning |
  |---|---|---|
  | 24 | `0x0100_0000` | Shift held |
  | 25 | `0x0200_0000` | Ctrl held |
  | 26 | `0x0400_0000` | Alt held |
  | 27 | `0x0800_0000` | Super held |

  Rules: (1) Shift is set for every non-printable code (arrows, Home, F-keys,
  Enter, Tab, Delete...) and omitted for a printable character because its
  case/symbol already reflects Shift, **unless Ctrl or Alt is also held**.
  (2) **Ctrl+letter is the lowercase letter plus the Ctrl bit** (`'c'|CTRL`),
  never a C0 control code, so Ctrl+H/I/M are distinct from Backspace/Tab/Enter
  (which are `8`/`9`/`13`, with the Ctrl bit if Ctrl is held: Ctrl+Backspace is
  `8|CTRL`). Ctrl+Shift+Z is `'z'|CTRL|SHIFT`. Letters use the *physical* key
  under the active layout (AZERTY: the key labelled `a` gives `'a'`). Ctrl with
  digits/symbols is the character plus the Ctrl bit. (3) Alt+letter is the
  letter plus the Alt bit. (4) F-keys, Delete and Insert are compositor-bound
  only; with no compositor they are dropped (the kernel terminal never sees
  them). (5) A client wanting the character for text input uses
  `code` when `is_printable(code)` and no Ctrl/Alt bit is set; `Enter`/`Tab`/
  `Backspace` are text characters `
`/`	`/`` by convention. Keys the
  compositor keeps: Alt+Tab, Ctrl+Tab (cycle windows; a plain Tab now goes to
  the focused client), Ctrl+Esc/Super (start menu, sent to the shell), Alt+F4
  (close window), Escape while Alt+Tab, a drag or a resize is active. `user::messenger::display::key`
  mirrors every constant plus `with_modifiers`/`CODE_MASK`.
- Issue #153 adds the first windowed system-state viewers on that backend:
  `sysmon` renders the syscall-14 snapshot (frame/slab/heap gauges, uptime, the
  task table) on its Overview tab and, on its Services tab (issue #489), the
  services `init` supervises with `healthd`'s health for each, and `fabricmon` renders the syscall-5 fabric (registry names with
  owners/interfaces, topics-broker counts, buffers/fences/handles, per-task
  usage). Each is one owner-drawn node with a one-second `ui` timer and `r`/`q`
  keys (`sysmon` adds `o`/`s` and clickable tabs), prints `SYSMON:*`/`FABMON:*` serial markers, and is captured in
  `.github/workflows/xui.yml` as the display owner in turn (both over the
  `LAZYOS_SERVICES=1` session, so the registry, broker and supervisor are
  live).
- Window management (issue #143) lives in `xuid`: the `surfaces` vector is the
  z-order (tail paints last), a title-bar press drags the window (which may hang
  off the left, right and bottom edges, keeping `TITLE_REACHABLE_W` of its
  title bar on screen), and the title bar carries close/minimize buttons.
  `xuid` paints no taskbar, clock or menu of its own (issue #157): those are
  LazyShell's panels (see *LazyShell* below). Minimized surfaces are hidden and
  restored from the shell's taskbar (`ActivateSurface`) or from Alt+Tab, which
  lists them too, so they stay reachable with no shell; `Tab` cycles focus
  skipping minimized ones. The close button sends the client a one-way
  `WindowClose` (method 10) event, which `xdemo` treats as "exit". Only
  `Commit` uses per-surface damage; WM layout changes repaint the full screen
  (a drag repaints the union of the old/new window rectangles). Within the
  damage, repaint paints each layer only where no opaque layer above it (window,
  panel, Alt+Tab panel) lies, so hidden windows cost nothing (#360).
- **Input during animations.** The minimize/restore/maximize/open zooms
  (`xuid/anim.rs`) are a short blocking loop of frames. Each frame reads the
  pending input itself (the kernel display queue, and `inputd`'s
  `PointerEvent`s while it owns the pointer) into a preallocated queue on the
  `Compositor` (`xuid/held.rs`): nothing is dropped or reordered, the main loop
  handles the held events before any newer input, and the frame draws the
  cursor, above the wireframe, at the newest pointer position, so the pointer
  never freezes. Boot self-test `XUID:HELD:PASS`.

**Resize and maximize**

A window is fixed-size until its client calls `SetSizeHints` (method 32). That
call declares content-size bounds (clamped to the compositor's minimums and the
screen; a `max` of 0 means the screen) and is refused for anyone but the
creator (`EACCES`), an unknown surface (`ENOENT`) or a min above its max
(`EINVAL`). A resizable window gets a third title-bar button (maximize /
restore, between close and minimize) and interactive resize edges.

- **Edges.** `xuid/geometry.rs` hit-tests the frame: a 4 px grip (2 px outside,
  4 px inside) on each side, widened to 14 px at the corners; the title bar's
  body stays a move handle and the button group is excluded. The pure rules
  (hit-testing, resize clamping, off-screen reachability, the maximized
  rectangle, size-hint validation) are self-tested at boot as
  `XUID:GEOM:PASS`.
- **Outline resize.** A press on an edge consumes the press and starts a
  `ResizeDrag`; moving the pointer composes the old and new outline rectangles
  and draws a wireframe (`xuid/anim.rs::outline`) without re-rendering the
  window. On release the new geometry is adopted and the client gets a one-way
  `Configure(width, height, Normal)` (method 33) plus a shell `Resized` event.
  `Escape` cancels and erases the outline; a destroyed surface clears the drag.
  Resize and title-bar drag are mutually exclusive and neither starts during a
  drag & drop or Alt+Tab.
- **Buffer decoupling.** A `Mapping` records the content size it was attached
  for and `Surface` mirrors it in `buf_w`/`buf_h`. `draw_surface` blits the
  buffer at that size (cropped to the content) and fills the uncovered strip
  with the window background, so the window may be resized before the client
  attaches the new buffer without a stride-mismatched read. `try_attach`
  accepts a buffer of at least `width * height * 4` bytes for the current
  size: a stale smaller buffer fails with `EINVAL`, while a stale larger one
  (attached while a shrink was in flight) is accepted and drawn at the current
  stride, one wrong frame, until the client handles its `Configure` and
  attaches again.
- **Maximize.** The maximize button or a double-click on the title bar toggles
  between the work area and the saved normal rectangle, using the same
  wireframe zoom as minimize; the client is told with `Configure(.., Maximized)`
  and a shell `Maximized`/`Unmaximized` event. A maximized window has no resize
  edges and does not move on a title drag, and it stays maximized across
  minimize/restore. When the shell sets a new work area or goes away (the work
  area returns to the whole screen), `reflow_maximized` re-fits every
  maximized window to it.
- **Client-requested size (`RequestSize`, method 34).** A surface that declared
  `SetSizeHints` (and is not maximized/minimized) may ask for a content size;
  `xuid` keeps the top-left corner, clamps to the hints and the screen, and
  replies with `Configure` carrying the applied size (even if unchanged). Used
  by the `sysmon`/`fabricmon` compact toggle; see `docs/window-resize-plan.md`.
- **Dead clients (`Ping`, method 35).** A client that is killed or crashes never calls
  `DestroySurface`. About once a second `xuid` sends every surface a one-way
  `Ping` on its event endpoint; a send that fails with `EPIPE` means the peer
  is gone, so `xuid` removes the surface exactly as `DestroySurface` would
  (`forget_surface`: shell `Destroyed`, focus, drag and Alt+Tab state, closed
  endpoint, released buffers) and repaints once (`reap.rs`, serial marker
  `XUID:REAP:SURFACE:<id>`). Clients ignore the event. Boot self-test
  `XUID:REAP:PASS`; session `tools/screenshot/examples/xui_reap.json` (needs a
  BusyBox image: `LAZYOS_DESKTOP=1 LAZYOS_XUI_AUTOSTART=term`; `init` restarts the
  killed Counter, so the window that reappears is a new surface).
- **Off-screen movement.** A title drag clamps the origin so at least
  `TITLE_REACHABLE_W` pixels of the title bar stay on screen horizontally and
  the title bar never goes above the work-area top or below its bottom; the
  body may hang off the left, right and bottom. Damage rectangles are
  intersected with the screen before composing or presenting, and the minimize
  animation's small rectangle is clamped on screen.

**Drag & drop (issue #145)**

`os.lazy.display.v1` gains additive methods (11–17) that move a typed payload
between surfaces through the compositor, while `clipboardd` stays the data
broker. The wire reuses the clipboard's offer/token model, so the
compositor never sees payload bytes (field names below are the IDL parameters):

| # | Method | Direction | Fields |
|---|---|---|---|
| 11 | `DragStart` | app → compositor | `surface`, `token`, `mime` |
| 12 | `DragCancel` | app → compositor | `surface` |
| 13 | `DragEnter` | compositor → app | `x`/`y` = surface-local, `mime` |
| 14 | `DragOver` | compositor → app | `x`/`y` = surface-local |
| 15 | `DragLeave` | compositor → app | – |
| 16 | `Drop` | compositor → app | `x`/`y` = surface-local, `token`, `mime` |
| 17 | `DragEnded` | compositor → source | `dropped` (bool) |

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
- `dragdemo` (`/system/bin/dragdemo`) is the evidence pair. The kernel boots it in the
  `LAZYOS_XUID=1` path; with no manifest argument it is a launcher and starts a
  `source` and a `target` child (one clipboard session). It logs
  `DND:START:PASS`, `DND:DROP:PASS`, `DND:CANCEL:PASS` and `DND:DENIED:PASS`
  (the target re-tries its dropped token from another session and expects the
  clipboard's refusal).

**Shell protocol (issue #167, S5.0)**

LazyShell (S5) is an ordinary `os.lazy.display.v1` client, so `xuid` grows an
set of methods and one-way events; unknown fields and methods are ignored by
older peers, and the no-shell sessions above are unchanged.

| # | Method | Direction | Fields |
|---|---|---|---|
| 18 | `ListSurfaces` | shell → compositor | reply: `surfaces`, an `Array<SurfaceRow>` (`id`, `title`, `x`/`y`/`w`/`h`, `minimized`, `focused`, `role`: 0 window, 1 desktop, 2 panel), desktop first, panels last |
| 19 | `GetWorkArea` | shell → compositor | reply: `x`/`y`/`w`/`h` available to windows |
| 20 | `Subscribe` | shell → compositor | `subscriber_role` string + transferred event endpoint |
| 21 | `GetTheme` | shell or app → compositor | reply: `title_bg_active`, `title_bg_inactive`, `border`, `taskbar`, `text` (ink on the inactive title) as `0xRRGGBB`, `mode` (`dark`/`light`) and `accent`; xui apps map the last two onto their widget theme |
| 22 | `SurfaceChanged` | compositor → shell | `surface`, `kind` (`Change`: created/destroyed/moved/minimized/restored/title), geometry + flags, `role`, optional `title` on create |
| 23 | `FocusChanged` | compositor → shell | `surface` (optional; absent = none) |
| 24 | `StartMenu` | compositor → shell | – (the Ctrl+Esc/Super hotkey fired) |
| 36 | `PlaceSurface` | panel creator → compositor | `surface`, `x`/`y` (clamped on screen) |
| 37 | `ActivateSurface` | shell → compositor | `surface`: restore (zoom), raise, focus |
| 38 | `MinimizeSurface` | shell → compositor | `surface`: like its minimize button |
| 39 | `SetWorkArea` | shell → compositor | `x`/`y`/`w`/`h`, clipped to the screen; empty is `EINVAL` |
| 40 | `SetIconGeometry` | shell → compositor | `surface`, `x`/`y`/`w`/`h`: its taskbar entry, the minimize/restore zoom target |
| 41 | `HintLaunchOrigin` | shell → compositor | `x`/`y`/`w`/`h`: the next window any task opens within 2 s zooms from it |
| 42 | `Dismiss` | compositor → shell | – (a press landed outside every panel) |

- **Roles.** `CreateSurface` gains a `role` (`0` window, the default when
  absent; `1` desktop; `2` panel, issue #157). A desktop surface paints at the
  bottom of the z-order — above the background colour, below every window —
  with no chrome, no Alt+Tab entry, and it never takes focus. Creating a new
  desktop replaces the previous one. A panel (the shell's taskbar, start menu)
  is chromeless and painted above every window, below the Alt+Tab overlay, the
  drag ghost and the cursor; panels stack in creation order, open at `(0, 0)`
  and are moved with `PlaceSurface` (creator only). Panels are never focused,
  never in Alt+Tab and not registered with `inputd` for keys; at most 16 exist
  (`EBUSY`).
- **Pointer on the shell's layers** (`xuid/layers.rs`). A panel or the
  desktop (where no window covers it) gets the pointer events while the
  pointer is over it, hover moves included; when the pointer leaves it gets one
  last `PointerMove(-1, -1)` so hover clears. A press on one grabs the pointer
  to it until every button is released. A press on a panel never changes
  window focus; any other press sends the shell `Dismiss`, so it closes its
  popups.
- **Authorization** (issues #157, #447). `Subscribe("shell")` is accepted
  from uid 0 or `CAP_SETUID`, or from a task whose kernel-stamped
  `cred.session` is non-zero and is the session that owns the display: the
  first one accepted as the shell, recorded then. Before any session owns it,
  an unprivileged claim is only taken while no live shell holds the role. A
  shell from the owning session replaces the previous one (a restarted
  LazyShell). Any other role is a separate observer slot (same events) that
  needs uid 0 or `CAP_SETUID` and never displaces the shell. Desktop and panel
  `CreateSurface`, `ListSurfaces` and methods 37-41 are shell-only (the task
  holding the subscription, or a privileged one); anyone else gets `-EACCES`
  (a transferred handle is closed). The pure rules are boot-tested
  (`XUID:SHELLCALLS:PASS`).
- **When the shell goes away.** The shell and observer endpoints are pinged
  with the surfaces about once a second; a dead (`EPIPE`) or replaced-by-another
  task shell is dropped, the work area returns to the whole screen and
  maximized windows are re-fitted. Its desktop and panels are reaped like any
  dead client's surfaces; every window stays. With no shell `xuid` paints the
  background and the windows, `Ctrl+Esc`/`Super` do nothing, and Alt+Tab
  still reaches every window (minimized ones included, restored on commit).
- **Window management for the taskbar.** `ActivateSurface` restores (with the
  zoom), raises and focuses a window; `MinimizeSurface` is the minimize button;
  both refuse a desktop or panel (`EINVAL`). `SetIconGeometry` records the
  window's taskbar entry: minimize, restore and the open zoom fly to and from
  it, or to a small rectangle at the bottom-left without one.
  `HintLaunchOrigin` is a global open-origin hint for the next window any task
  creates within two seconds (the menu row or icon that launched it); a task's
  own `HintOpenOrigin` wins over it.
- **Global hotkeys** are modifier-aware and compositor-owned: the kernel
  forwards Shift/Ctrl/Alt/Super press/release plus F-keys to the bound
  compositor (`display::key` 0x108-0x10B, 0x113), and `xuid` consumes them.
  `Alt+Tab` opens a centered window-title overlay, repeated Tab cycles the
  selection, releasing Alt restores/raises/focuses the selection and emits
  `FocusChanged`; `Ctrl+Esc` and `Super` send `StartMenu`; `Alt+F4` sends
  `WindowClose` to the focused surface exactly like its `X` button; `Escape`
  still cancels a drag & drop.
- The desktop context menu, taskbar and clock that `xuid` used to paint
  (issues #323, #370) moved to LazyShell (issue #157, `xui-app`), which builds
  them from these calls.
- `shellprobe` (`/system/bin/shellprobe`) is the evidence client. It claims the display
  for its session, registers as the `"shell"` subscriber, creates a full-screen
  desktop, reads back `ListSurfaces`/`GetWorkArea`/`GetTheme`, creates one
  window, and logs `SHELLPROBE:DESKTOP:PASS`, `SHELLPROBE:LIST:PASS`,
  `SHELLPROBE:FOCUS:PASS` (first `FocusChanged`) and `SHELLPROBE:HOTKEY:PASS`
  (first `StartMenu`). The LazyShell calls (`shellprobe/checks.rs`):
  `SHELLPROBE:PANEL:PASS` (a panel placed top-right, clamped on screen, listed
  last), `SHELLPROBE:WORKAREA:PASS` (`SetWorkArea` round-trip, empty refused),
  `SHELLPROBE:WM:PASS` (icon geometry, minimize, activate, launch hint, and the
  wrong-role/unknown-id errors) and `SHELLPROBE:EVICT:PASS` (a privileged
  child subscribing as an observer leaves the shell's work area and event
  channel intact). An unprivileged child in another session
  (`/system/bin/shellprobe denied <panel>`) must be refused the shell role, an observer
  slot, a desktop and the list (`SHELLPROBE:DENIED:PASS`), and a panel, every
  shell-only call and moving the probe's panel (`SHELLPROBE:SHELLONLY:PASS`).
  The probe also logs the pointer events its desktop and panel receive
  (`SHELLPROBE:DESKTOP:DOWN|UP|LEAVE|WHEEL`, `SHELLPROBE:PANELPTR:*`) and each
  `SHELLPROBE:DISMISS`. It boots only with `LAZYOS_XUID=1` plus the
  `LAZYOS_SHELLPROBE=1` demo hook, so the default compositor sessions keep
  their window layout.

**Invariants.** Mux is always the fallback: no compositor state is required to
paint. The screen buffer handoff app-to-compositor is zero-copy (shared
buffers); only the final composite is a memcpy per damage rectangle. Input
events go only to the bound compositor; `push_event` drops the oldest event when
the queue is full and is IRQ-safe (leaf lock). The shell is a pure observer of
the surface table: the desktop role and `Subscribe` never hand the shell
compositor state, and a shell that exits leaves the fallback compositor usable.

**Status.** Working: demo mux, display grant, xuid + xdemo in headless captures
(`LAZYOS_XUID=1`), xuid window management (drag, z-order, buttons,
focus cycling; `XUID:WM:PASS`), compositor-mediated drag & drop with a
clipboard-token transfer (`dragdemo`; `DND:*:PASS`), the shell protocol
(desktop role, surface list/work-area/theme read-backs, shell events, global
Alt+Tab/Ctrl+Esc/Alt+F4 hotkeys; `SHELLPROBE:*:PASS`), the xui app milestones
M0-M2 (`XUIAPP:*:PASS`), the sysmon/fabricmon viewers (`SYSMON:*`/`FABMON:*`
markers, screenshots in the `xui-app` workflow), compositor client mode
(`xui-client` inside a `xuid` window; `XUIAPP:CLIENT:PASS`, `XUIAPP:KEY:PASS`,
`XUIAPP:CLOSE:PASS`) and keyboard focus routing (click-focus, Tab /
PageDown cycling, keys to the focused widget). Open: zero-copy scanout,
userspace XUI toolkit,
multi-session compositors, drag targets that can refuse a drop before release.

**Pipelined present (issue #361)**

Methods 25-28 add a tear-free, paced alternative to the blocking `Commit`
(which keeps working unchanged). A surface has up to four buffer slots:
`AttachBufferSlot(surface, slot)` (25) registers one (`EBUSY` if it is the
current slot); the one-way `Present(surface, slot, seq, damage)` (26) makes a
slot current and composites the clipped damage (empty or more than 16 rects =
whole surface). The compositor only reads the current slot, so the others are
safe to draw into. After compositing it sends `BufferRelease(surface, slot)`
(27) for the slot it just stopped reading, then `FrameDone(surface, seq)` (28),
on the surface's event endpoint; a refused present still gets its `FrameDone`,
and a present from a non-owner is dropped. The rules live in
`libs/surfbuf` (`SlotTable` for the compositor, `Swapchain` for the client),
exercised by `display_slots_*` in the kernel suite; `xdemo` is the reference
double-buffered client. The legacy `AttachBuffer` is "slot 0, current at once".

Every `xui-app` window presents this way (issue #372,
`xui-app/src/client_window/slots.rs`): two slots, a frame drawn only while
one is free (so the compositor's `BufferRelease` paces repaints and the app
never blocks on a reply), and a slot reallocated at the window size when it
is next drawn after a `Configure`. The backend repaints only the window's
accumulated damage rectangle (issue #487): it clears it, runs just the
painters of nodes within two pixels of it, unclipped (a `SkiaCanvas` clip
trims shapes before stroking them, which would draw borders along the damage
edge), and copies only that rectangle into the window's composed frame. Each
slot tracks which of its pixels are older than that frame, so filling a slot
copies the damage of the frame it missed as well as the current one. A new window, a resize and a
theme change damage the whole window. `dragdemo` and `shellprobe` stay on
`AttachBuffer`/`Commit`: `dragdemo` redraws only on a drop, and `shellprobe`
is what exercises the legacy path.

**Retitling a window (`SetTitle`, method 29)**

`SetTitle(surface, title)` renames a window after creation, so a document app
can show the file it holds (the Editor shows `note.txt - Editor`, `*` prefixed
while modified). Only the surface's creator may call it (`EACCES` otherwise,
`ENOENT` for an unknown surface). `xuid` (`user/src/bin/xuid/title.rs`) keeps at
most 128 bytes cut at a character boundary, drops control characters, trims the
result and keeps the old title if nothing printable is left; an unchanged title
is a no-op. A change repaints the chrome, and sends the shell a
`SurfaceChanged` event of kind `Title` whose `title` field carries the new
name. The boot self-test prints `XUID:TITLE:PASS` (`title::selftest_titles`).
Clients that never call it keep their `CreateSurface` title; a client talking
to a compositor that predates the method gets `EINVAL`, which `xui-app` ignores.

**Opening from a tile (`HintOpenOrigin`, method 30)**

`HintOpenOrigin(surface, x, y, w, h)` tells the compositor where the task's
*next* `CreateSurface` should zoom open from, in place of its icon rectangle:
the rectangle is relative to the content origin of `surface`, which the caller
must own (`EACCES` otherwise, `ENOENT` if unknown). Files sends it when a
folder tile is double-clicked, so the wireframe leaves the tile. It is cosmetic
and untrusted: `xuid` (`user/src/bin/xuid/origin.rs`) translates it to screen
coordinates with 64-bit arithmetic, clamps it to the screen, ignores an empty or
off-screen rectangle (and one from a minimized surface), keeps one hint per
task, consumes it at that task's next `CreateSurface` and drops it after two
seconds. With a hint the animation is a single wireframe zoom from the rectangle
to the window (`Compositor::open_zoom`); without one the shell's
`HintLaunchOrigin` applies, else the window's icon rectangle
(`SetIconGeometry`). Placement and focus are never affected. An
older compositor answers `EINVAL`, which `xui-app` ignores. The boot self-test
prints `XUID:ORIGIN:PASS` (`origin::selftest_open_origin`).

**Focus on create**

A newly created non-desktop surface is raised to the top of the paint order and
focused (`window::focus_on_create`), so a window that just opened comes up in
front of the one that spawned it instead of behind it (the Files app
double-clicking a folder opens a new window). The first window still gets
focus; focusing the surface that already holds it sends no `FocusChanged`. The
boot self-test prints `XUID:FOCUS:PASS` (`window::selftest_focus_on_create`).
Only the shell may raise or focus an *existing* window (`ActivateSurface`), so
the portable explorer still reports a duplicate open in its status bar rather
than bringing the open window forward.
