# `sys::display`

The Rhai module for the Messenger interface `os.lazy.display.v1`. Wire-level reference: [os.lazy.display.v1](../../idl/os.lazy.display.v1.md).

The userspace compositor protocol (`xuid`; issues #113, #143, #145, #167,
#287).

One interface carries both directions. **Calls** (app or shell to
compositor) are ordinary request/reply methods: `CreateSurface`,
`AttachBuffer`, `Commit`, `DestroySurface`, `DragStart`, `DragCancel`,
`ListSurfaces`, `GetWorkArea`, `Subscribe`, `GetTheme`, `SetTitle`,
`HintOpenOrigin` and `SetSizeHints`. **Events** (compositor to app, or to
the shell subscriber) are `oneway` methods sent on the event endpoint the
client transferred: `PointerMove`, `PointerDown`, `PointerUp`,
`PointerWheel`, `KeyDown`, `KeyUp`, `WindowClose`, `Configure`, the
drag-and-drop set
`DragEnter`/`DragOver`/`DragLeave`/`Drop`/`DragEnded`, and the shell set
`SurfaceChanged`/`FocusChanged`/`StartMenu`. Method ids are pinned to the
values the hand-written protocol used (1-24), so the numbering stays
append-only from here on.

Endpoints and buffers are object fields of the request (the parcel's
object list, which the kernel resolves and installs in the receiver's
table): `CreateSurface` and `Subscribe` move one event endpoint
(`events`), `AttachBuffer` shares one pixel buffer (`pixels`, at least
`width * height * 4` bytes of RGBA8 for the surface's current content
size, rows tightly packed at that width).

Pointer coordinates in every event are relative to the surface content
origin; a move outside the surface (a press-and-drag) reports negative or
oversized values. Pointer events carry a button id (`1` left, `2` right,
`3` middle). Failures of calls are returned as a structured error field
(id 15, errno-style code plus friendly text) instead of the declared reply
fields, which never use that id.

## Functions

### `commit(surface, x, y, w, h)`

`Commit(surface: U64, x: U32, y: U32, w: U32, h: U32) -> ()`

Signal that the damage rectangle of `surface` (content-relative) is
ready to present.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `surface` | `U64` | int |
| `x` | `U32` | int |
| `y` | `U32` | int |
| `w` | `U32` | int |
| `h` | `U32` | int |

Returns `()`.

```rhai
let result = sys::display::commit(surface, x, y, w, h);
```

### `destroy_surface(surface)`

`DestroySurface(surface: U64) -> ()`

Drop a surface; the compositor forgets it and repaints.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `surface` | `U64` | int |

Returns `()`.

```rhai
let result = sys::display::destroy_surface(surface);
```

### `pointer_move(x, y)`

`PointerMove(x: I32, y: I32) -> () oneway`

Event: the pointer moved to `(x, y)`, relative to the focused surface.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `x` | `I32` | int |
| `y` | `I32` | int |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::display::pointer_move(x, y);
```

### `pointer_down(x, y, button)`

`PointerDown(x: I32, y: I32, button: U32) -> () oneway`

Event: pointer button `button` went down at `(x, y)`, relative to the
surface.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `x` | `I32` | int |
| `y` | `I32` | int |
| `button` | `U32` | int |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::display::pointer_down(x, y, button);
```

### `pointer_up(x, y, button)`

`PointerUp(x: I32, y: I32, button: U32) -> () oneway`

Event: pointer button `button` went up at `(x, y)`, relative to the
surface.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `x` | `I32` | int |
| `y` | `I32` | int |
| `button` | `U32` | int |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::display::pointer_up(x, y, button);
```

### `key_down(key)`

`KeyDown(key: U32) -> () oneway`

Event: key `key` was pressed (a character or a non-printable code).

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `key` | `U32` | int |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::display::key_down(key);
```

### `key_up(key)`

`KeyUp(key: U32) -> () oneway`

Event: key `key` was released.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `key` | `U32` | int |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::display::key_up(key);
```

### `window_close()`

`WindowClose() -> () oneway`

Event: the window manager closed this surface (the X button or Alt+F4).
The app is expected to exit or re-create it.

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::display::window_close();
```

### `drag_start(surface, token, mime)`

`DragStart(surface: U64, token: U64, mime: String) -> ()`

Begin a compositor-mediated drag from `surface` carrying clipboard
`token` of type `mime`. Only the surface's creator may start one, while
a pointer button is held; one drag at a time.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `surface` | `U64` | int |
| `token` | `U64` | int |
| `mime` | `String` | string |

Returns `()`.

```rhai
let result = sys::display::drag_start(surface, token, mime);
```

### `drag_cancel(surface)`

`DragCancel(surface: U64) -> ()`

Cancel the drag that started at `surface`.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `surface` | `U64` | int |

Returns `()`.

```rhai
let result = sys::display::drag_cancel(surface);
```

### `drag_enter(x, y, mime)`

`DragEnter(x: I32, y: I32, mime: String) -> () oneway`

Event: a drag carrying `mime` entered this surface at `(x, y)`.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `x` | `I32` | int |
| `y` | `I32` | int |
| `mime` | `String` | string |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::display::drag_enter(x, y, mime);
```

### `drag_over(x, y)`

`DragOver(x: I32, y: I32) -> () oneway`

Event: a drag moved inside this surface to `(x, y)`.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `x` | `I32` | int |
| `y` | `I32` | int |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::display::drag_over(x, y);
```

### `drag_leave()`

`DragLeave() -> () oneway`

Event: a drag left this surface.

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::display::drag_leave();
```

### `drop(x, y, token, mime, modifiers, source)`

`Drop(x: I32, y: I32, token: U64, mime: String, modifiers: Option<U32>, source: Option<U64>) -> () oneway`

Event: a drag was released over this surface at `(x, y)`; the target
pastes `token` of type `mime` through the clipboard service.
`modifiers` are the `MOD_*` key bits held at the release (the
keyboard may be another client's, so the target cannot read them
itself) and `source` is the surface the drag started from, so a
client can tell a drag between its own windows from another app's.
Both are absent from an older compositor.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `x` | `I32` | int |
| `y` | `I32` | int |
| `token` | `U64` | int |
| `mime` | `String` | string |
| `modifiers` | `Option<U32>` | int or () |
| `source` | `Option<U64>` | int or () |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::display::drop(x, y, token, mime, modifiers, source);
```

### `drag_ended(dropped)`

`DragEnded(dropped: Bool) -> () oneway`

Event to the drag's source: the drag ended (`dropped`) or was cancelled.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `dropped` | `Bool` | bool |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::display::drag_ended(dropped);
```

### `list_surfaces()`

`ListSurfaces() -> (surfaces: Array<SurfaceRow>)`

Every surface in z-order (bottom first), including the desktop and
panels. Shell-only (the `shell` subscriber's task, uid 0 or
`CAP_SETUID`).

Returns `surfaces`: `Array<SurfaceRow>` (array of map (`SurfaceRow`)).

```rhai
let result = sys::display::list_surfaces();
```

### `get_work_area()`

`GetWorkArea() -> (x: I32, y: I32, w: I32, h: I32)`

The rectangle available to windows: what the shell set with
`SetWorkArea`, or the whole screen (xuid paints no desktop UI of its
own since issue #157).

Returns a map:

| Name | IDL type | Script value |
|---|---|---|
| `x` | `I32` | int |
| `y` | `I32` | int |
| `w` | `I32` | int |
| `h` | `I32` | int |

```rhai
let result = sys::display::get_work_area();
```

### `get_theme()`

`GetTheme() -> (title_bg_active: U32, title_bg_inactive: U32, border: U32, taskbar: U32, text: U32, mode: String, accent: U32)`

The compositor's chrome palette as `0xRRGGBB` colours. `text` is the
ink on the inactive title bar. `mode` is the desktop preset (`dark` or
`light`, the `sys/ui/mode` setting) and `accent` the accent colour in
effect, so an app can match its own widgets to the desktop.

Returns a map:

| Name | IDL type | Script value |
|---|---|---|
| `title_bg_active` | `U32` | int |
| `title_bg_inactive` | `U32` | int |
| `border` | `U32` | int |
| `taskbar` | `U32` | int |
| `text` | `U32` | int |
| `mode` | `String` | string |
| `accent` | `U32` | int |

```rhai
let result = sys::display::get_theme();
```

### `surface_changed(surface, kind, x, y, w, h, minimized, focused, title, role, maximized)`

`SurfaceChanged(surface: U64, kind: U32, x: I32, y: I32, w: I32, h: I32, minimized: Bool, focused: Bool, title: Option<String>, role: U32, maximized: Bool) -> () oneway`

Shell event: surface `surface` changed. `kind` is a `Change` value;
`title` is set on `Created` and `Title`. `role` is a `Role` value.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `surface` | `U64` | int |
| `kind` | `U32` | int |
| `x` | `I32` | int |
| `y` | `I32` | int |
| `w` | `I32` | int |
| `h` | `I32` | int |
| `minimized` | `Bool` | bool |
| `focused` | `Bool` | bool |
| `title` | `Option<String>` | string or () |
| `role` | `U32` | int |
| `maximized` | `Bool` | bool |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::display::surface_changed(surface, kind, x, y, w, h, minimized, focused, title, role, maximized);
```

### `focus_changed(surface)`

`FocusChanged(surface: Option<U64>) -> () oneway`

Shell event: the focused surface changed; absent when nothing is
focused.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `surface` | `Option<U64>` | int or () |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::display::focus_changed(surface);
```

### `start_menu()`

`StartMenu() -> () oneway`

Shell event: the global start-menu hotkey (Ctrl+Esc or Super) fired.

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::display::start_menu();
```

### `present(surface, slot, seq, damage)`

`Present(surface: U64, slot: U32, seq: U64, damage: Array<Rect>) -> () oneway`

Pipelined present (no reply): make `slot` the surface's current buffer
and composite `damage` (content-relative, clipped to the content; an
empty list or more than 16 rects means the whole content). `seq` is
echoed in `FrameDone`. After compositing, the compositor sends
`BufferRelease` for the slot this present replaced (if it changed) and
then `FrameDone`. A minimized surface still swaps its slot (so the
newest frame shows on restore and the client gets its release) but is
not repainted. A refused present (unattached or out-of-range slot)
changes nothing: the compositor sends `BufferRelease` for the
submitted slot, since it never read it, and then `FrameDone`. A
present from a task that does not own `surface` is dropped silently.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `surface` | `U64` | int |
| `slot` | `U32` | int |
| `seq` | `U64` | int |
| `damage` | `Array<Rect>` | array of map (`Rect`) |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::display::present(surface, slot, seq, damage);
```

### `buffer_release(surface, slot)`

`BufferRelease(surface: U64, slot: U32) -> () oneway`

Event: the compositor no longer reads `slot` of `surface`; the client
may write it again. Only sent to surfaces that use `Present`.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `surface` | `U64` | int |
| `slot` | `U32` | int |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::display::buffer_release(surface, slot);
```

### `frame_done(surface, seq)`

`FrameDone(surface: U64, seq: U64) -> () oneway`

Event: the compositor consumed the `Present` numbered `seq`. Frames
complete in submission order.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `surface` | `U64` | int |
| `seq` | `U64` | int |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::display::frame_done(surface, seq);
```

### `set_title(surface, title)`

`SetTitle(surface: U64, title: String) -> ()`

Rename `surface`'s window (its title bar, taskbar entry and switcher
row) to `title`, so a document window can show the file it holds. Only
the surface's creator may rename it (`EACCES` otherwise; `ENOENT` for
an unknown surface). The compositor keeps at most 128 bytes (cut at a
character boundary), drops control characters and keeps the previous
title when nothing printable is left, so a client cannot blank or
overflow its own chrome. A change sends the shell a `SurfaceChanged`
event of kind `Title`. Clients that never call it keep the
`CreateSurface` title, so old clients are unaffected.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `surface` | `U64` | int |
| `title` | `String` | string |

Returns `()`.

```rhai
let result = sys::display::set_title(surface, title);
```

### `hint_open_origin(surface, x, y, w, h)`

`HintOpenOrigin(surface: U64, x: I32, y: I32, w: U32, h: U32) -> ()`

Hint where the *next* `CreateSurface` from this task should open from:
a rectangle `(x, y, w, h)` relative to the content origin of `surface`
(which the caller must own; `EACCES` otherwise, `ENOENT` for an unknown
surface), typically the tile the user just double-clicked. The
compositor uses it only as the start of that window's opening zoom
animation, in place of the taskbar entry; it never affects placement,
focus or anything else. The rectangle is translated to screen
coordinates and clamped to the screen; an empty rectangle, or one
wholly off the screen, is ignored. There is at most one hint per task
(a new one replaces the old) and it is consumed by the next
`CreateSurface` from that task, or expires after about two seconds.
Minimize and restore keep using the taskbar entry. A compositor that
predates the method answers `EINVAL`, which callers ignore.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `surface` | `U64` | int |
| `x` | `I32` | int |
| `y` | `I32` | int |
| `w` | `U32` | int |
| `h` | `U32` | int |

Returns `()`.

```rhai
let result = sys::display::hint_open_origin(surface, x, y, w, h);
```

### `pointer_wheel(x, y, delta)`

`PointerWheel(x: I32, y: I32, delta: I32) -> () oneway`

Event: the mouse wheel rolled `delta` notches over this surface's
content at `(x, y)` (relative to the content origin). Positive scrolls
up (the wheel rolled away from the user), negative down. The compositor
sends it to the topmost window under the pointer, which need not be the
focused one, and never to the title bar or an area outside every window.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `x` | `I32` | int |
| `y` | `I32` | int |
| `delta` | `I32` | int |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::display::pointer_wheel(x, y, delta);
```

### `set_size_hints(surface, min_w, min_h, max_w, max_h)`

`SetSizeHints(surface: U64, min_w: U32, min_h: U32, max_w: U32, max_h: U32) -> ()`

Declare `surface` resizable within these content-size bounds (pixels).
Only the creator may call it (`EACCES`; `ENOENT` for an unknown
surface). A `max_*` of 0 means the largest size the screen allows.
Bounds are clamped to the compositor's minimums and the screen; a
resulting `min` greater than its `max` is `EINVAL`. Until it is called a
window is fixed-size: no resize edges and no maximize button, so old
clients are unaffected. The bounds apply to the content size (the
decorated window is `BORDER` wider and `TITLE_H + BORDER` taller).

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `surface` | `U64` | int |
| `min_w` | `U32` | int |
| `min_h` | `U32` | int |
| `max_w` | `U32` | int |
| `max_h` | `U32` | int |

Returns `()`.

```rhai
let result = sys::display::set_size_hints(surface, min_w, min_h, max_w, max_h);
```

### `configure(surface, width, height, state)`

`Configure(surface: U64, width: U32, height: U32, state: U32) -> () oneway`

Event: the window manager changed the surface's content size to
`width` x `height` (`state` is a `WindowState`). The client should
attach buffer(s) of the new size (`AttachBuffer`, or `AttachBufferSlot`
on non-current slots for `Present` users) and present a full frame.
Until it does the compositor shows the old buffer cropped or padded.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `surface` | `U64` | int |
| `width` | `U32` | int |
| `height` | `U32` | int |
| `state` | `U32` | int |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::display::configure(surface, width, height, state);
```

### `request_size(surface, width, height)`

`RequestSize(surface: U64, width: U32, height: U32) -> ()`

Ask the compositor to change `surface`'s content size to `width` x
`height`, keeping its top-left corner (a compact/expanded toggle). Only
the creator may call it (`EACCES`; `ENOENT` for an unknown surface) and
only a surface that declared `SetSizeHints` and is neither maximized nor
minimized (`EINVAL` otherwise). The size is clamped to the declared
bounds and to the screen; the compositor answers by sending a
`Configure` with the size it actually applied (also when that equals
the current size), which is the only confirmation the client needs.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `surface` | `U64` | int |
| `width` | `U32` | int |
| `height` | `U32` | int |

Returns `()`.

```rhai
let result = sys::display::request_size(surface, width, height);
```

### `ping()`

`Ping() -> () oneway`

Event: a liveness probe. The compositor sends it to each window's
event endpoint about once a second; a send that fails with `EPIPE`
tells it the client died without calling `DestroySurface`, and it
removes the window. Clients ignore it (an old client drops an unknown
event), so there is nothing to answer.

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::display::ping();
```

### `place_surface(surface, x, y)`

`PlaceSurface(surface: U64, x: I32, y: I32) -> ()`

Move a `Panel` surface so its top-left corner is at screen `(x, y)`.
Only the surface's creator may call it (`EACCES`), only for a panel
(`EINVAL` for any other role; `ENOENT` for an unknown surface). The
position is clamped so the panel stays on screen. A new panel opens at
`(0, 0)`, so a shell places it before its first `Commit`.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `surface` | `U64` | int |
| `x` | `I32` | int |
| `y` | `I32` | int |

Returns `()`.

```rhai
let result = sys::display::place_surface(surface, x, y);
```

### `activate_surface(surface)`

`ActivateSurface(surface: U64) -> ()`

Shell-only: restore `surface` if minimized, raise it and give it focus
(a taskbar entry click). `ENOENT` for an unknown surface, `EINVAL` for
a desktop or panel.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `surface` | `U64` | int |

Returns `()`.

```rhai
let result = sys::display::activate_surface(surface);
```

### `minimize_surface(surface)`

`MinimizeSurface(surface: U64) -> ()`

Shell-only: minimize `surface` (a click on the focused taskbar entry).
The zoom animation flies to the rectangle `SetIconGeometry` gave for
it. `ENOENT` for an unknown surface, `EINVAL` for a desktop or panel.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `surface` | `U64` | int |

Returns `()`.

```rhai
let result = sys::display::minimize_surface(surface);
```

### `set_work_area(x, y, w, h)`

`SetWorkArea(x: I32, y: I32, w: I32, h: I32) -> ()`

Shell-only: the rectangle windows may occupy (the screen minus the
shell's taskbar). Clamped to the screen; an empty result is `EINVAL`.
Maximized windows are re-fitted to it. When the shell goes away the
work area returns to the whole screen.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `x` | `I32` | int |
| `y` | `I32` | int |
| `w` | `I32` | int |
| `h` | `I32` | int |

Returns `()`.

```rhai
let result = sys::display::set_work_area(x, y, w, h);
```

### `set_icon_geometry(surface, x, y, w, h)`

`SetIconGeometry(surface: U64, x: I32, y: I32, w: I32, h: I32) -> ()`

Shell-only: where `surface`'s taskbar entry is on screen, so the
minimize/restore zoom flies to and from it. A window without one zooms
to a small rectangle at the bottom-left of the screen. `ENOENT` for an
unknown surface; an empty rectangle forgets the geometry.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `surface` | `U64` | int |
| `x` | `I32` | int |
| `y` | `I32` | int |
| `w` | `I32` | int |
| `h` | `I32` | int |

Returns `()`.

```rhai
let result = sys::display::set_icon_geometry(surface, x, y, w, h);
```

### `hint_launch_origin(x, y, w, h)`

`HintLaunchOrigin(x: I32, y: I32, w: U32, h: U32) -> ()`

Shell-only: the next window *any* task creates within about two seconds
zooms open from this screen rectangle (the start-menu row or desktop
icon the user just activated) instead of from its taskbar entry. The
shell sends it right before asking `init` to launch an app. A task's
own `HintOpenOrigin` wins over it. An empty rectangle is ignored.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `x` | `I32` | int |
| `y` | `I32` | int |
| `w` | `U32` | int |
| `h` | `U32` | int |

Returns `()`.

```rhai
let result = sys::display::hint_launch_origin(x, y, w, h);
```

### `dismiss()`

`Dismiss() -> () oneway`

Shell event: a pointer button went down outside every `Panel` surface
(on a window, the desktop or the bare background), so the shell closes
any open popup panel such as the start menu.

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::display::dismiss();
```

### `get_output()`

`GetOutput() -> (width: U32, height: U32, scale: U32, refresh_millihz: U32)`

The screen in physical pixels and the desktop's integer UI scale
(`1` or `2`; docs/hidpi-plan.md). Every size and coordinate on this
interface stays in physical pixels; a scale-aware client draws at
`scale` times its design size (an xui app runs at `96 * scale` DPI).
The scale is fixed for the compositor's lifetime. `refresh_millihz`
is how often the screen shows a new picture, in millihertz: the rate
`gpud` measured on a display it drives, else 60000 (no vblank to
follow, as under QEMU). A client presents at most that often
(xui paces its windows to it): a frame between two refreshes is
never seen, and composing it only costs the next one time.

Returns a map:

| Name | IDL type | Script value |
|---|---|---|
| `width` | `U32` | int |
| `height` | `U32` | int |
| `scale` | `U32` | int |
| `refresh_millihz` | `U32` | int |

```rhai
let result = sys::display::get_output();
```

### `allow_popup(task, token, x, y, w, h)`

`AllowPopup(task: U64, token: U64, x: I32, y: I32, w: U32, h: U32) -> ()`

Shell-only: let task `task` create one `Popup` surface with `token`
within about two seconds, placed against the screen rectangle
`(x, y, w, h)` (the tray icon the user clicked; docs/tray-plan.md
section 7.3). A token works once and only for that task. A compositor
without flyouts answers `EINVAL`.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `task` | `U64` | int |
| `token` | `U64` | int |
| `x` | `I32` | int |
| `y` | `I32` | int |
| `w` | `U32` | int |
| `h` | `U32` | int |

Returns `()`.

```rhai
let result = sys::display::allow_popup(task, token, x, y, w, h);
```

### `grab_panel_keys(grab)`

`GrabPanelKeys(grab: Bool) -> ()`

Shell-only: take the keyboard for the shell's panel menus (issue
#648; the start menu, its submenus, a tray menu, the tray). While
`grab` is set every key the compositor does not keep for itself
(Alt+Tab, Alt+F4, Ctrl+Esc, Super, Ctrl+Alt+Esc) goes to the shell as
a `PanelKey` event instead of to the focused window, which keeps its
focus (no leave/enter); `inputd` holds the window's key content
meanwhile (`NoteKeysHeld` on `os.lazy.input.shell.v1`). It ends
with `GrabPanelKeys(false)`, when the shell's subscription goes away,
or under a client's keyboard grab or the trusted prompt, which win.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `grab` | `Bool` | bool |

Returns `()`.

```rhai
let result = sys::display::grab_panel_keys(grab);
```

### `panel_key(key)`

`PanelKey(key: U32) -> () oneway`

Shell event: a key went down while the shell holds the panel keys
(`GrabPanelKeys`), with the held modifiers OR-ed in like `KeyDown`.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `key` | `U32` | int |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::display::panel_key(key);
```

### `tray_keys()`

`TrayKeys() -> () oneway`

Shell event: Super+B (Win+B) went down: move the keyboard to the
tray. Super pressed and released alone still sends `StartMenu`.

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::display::tray_keys();
```

### `set_close_mode(surface, ask)`

`SetCloseMode(surface: U64, ask: Bool) -> ()`

Ask the compositor to keep `surface` when the window manager closes it
(the X button, Alt+F4, the taskbar): `WindowClose` is still sent, but
the window stays until the app destroys the surface, so an app that must
ask first (a terminal with a program running) can put its question in
the window. Without it (the default, and for every older client) the
window is dropped as it is sent. Only the creator may call it.
An app can never make its window unclosable: the first close is passed
on and the window kept; if the app has not destroyed the surface three
seconds later (and within 30 s of the first), the next close drops it
whatever the app says (the app's event endpoint closes, so it quits);
only a close more than 30 s after the last one is passed on again.
Calling `SetCloseMode` again changes nothing about that. A shutdown or
logout never depends on it: `init` kills what is left.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `surface` | `U64` | int |
| `ask` | `Bool` | bool |

Returns `()`.

```rhai
let result = sys::display::set_close_mode(surface, ask);
```

### `activate_app(label, session)`

`ActivateApp(label: U32, session: U64) -> ()`

Shell-only: restore, raise and focus every window created by a program
running under the package label `label` in `session`, the one that was
topmost ending up focused (a click on one of that app's notifications:
`os.lazy.notify.events.v1` `Activated`, which the shell also sends the
app). Both numbers are what the kernel stamped on the notification
call, never ones an app chose, and they name the *app*, so the window
of a program that notifies from another thread than the one that made
its window still comes forward. An unlabelled program has no label to
name (`EINVAL`); `ENOENT` when the app owns no window. Desktops and
panels are never touched.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `label` | `U32` | int |
| `session` | `U64` | int |

Returns `()`.

```rhai
let result = sys::display::activate_app(label, session);
```

Not callable from a script (the request carries a kernel object): `CreateSurface`, `AttachBuffer`, `Subscribe`, `AttachBufferSlot`.

## Types

### `Rect`

A damage rectangle relative to a surface's content origin.

| Name | IDL type | Script value |
|---|---|---|
| `x` | `U32` | int |
| `y` | `U32` | int |
| `w` | `U32` | int |
| `h` | `U32` | int |

```rhai
let value = sys::display::new_rect();
```

### `SurfaceRow`

One row of a `ListSurfaces` reply.

| Name | IDL type | Script value |
|---|---|---|
| `id` | `U64` | int |
| `title` | `String` | string |
| `x` | `I32` | int |
| `y` | `I32` | int |
| `w` | `I32` | int |
| `h` | `I32` | int |
| `minimized` | `Bool` | bool |
| `focused` | `Bool` | bool |
| `role` | `U32` | int |
| `maximized` | `Bool` | bool |

```rhai
let value = sys::display::new_surface_row();
```

### `Role`

A string; the constants of this module are:

- `ROLE_WINDOW` = `"Window"`
- `ROLE_DESKTOP` = `"Desktop"`
- `ROLE_PANEL` = `"Panel"`
- `ROLE_POPUP` = `"Popup"`

`ROLE` is the array of all of them.

### `Change`

A string; the constants of this module are:

- `CHANGE_UNSPECIFIED` = `"Unspecified"`
- `CHANGE_CREATED` = `"Created"`
- `CHANGE_DESTROYED` = `"Destroyed"`
- `CHANGE_MOVED` = `"Moved"`
- `CHANGE_MINIMIZED` = `"Minimized"`
- `CHANGE_RESTORED` = `"Restored"`
- `CHANGE_TITLE` = `"Title"`
- `CHANGE_RESIZED` = `"Resized"`
- `CHANGE_MAXIMIZED` = `"Maximized"`
- `CHANGE_UNMAXIMIZED` = `"Unmaximized"`

`CHANGE` is the array of all of them.

### `WindowState`

A string; the constants of this module are:

- `WINDOW_STATE_NORMAL` = `"Normal"`
- `WINDOW_STATE_MAXIMIZED` = `"Maximized"`

`WINDOW_STATE` is the array of all of them.

## See also

- [Messenger APIs for Rhai](index.md)
- [msg: Messenger from Rhai](../msg.md)
- [os.lazy.display.v1 on the wire](../../idl/os.lazy.display.v1.md)
