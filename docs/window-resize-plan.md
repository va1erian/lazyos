# Resizable, maximizable and off-screen windows (`xuid`)

Status: implemented. Scope: the `xuid` compositor (`user/src/bin/xuid/`), the
`os.lazy.display.v1` interface (`idl/display.midl`), the display client
libraries (`user/src/messenger/display/`, `xui-app/src/display.rs`,
`xui-app/src/client_window.rs`) and the apps that opt in.

## Goals

1. **Off-screen movement.** A title-bar drag may push a window partly off the
   left, right and bottom edges of the screen. Only enough of the title bar to
   grab it again must stay reachable.
2. **Interactive resize.** Dragging a window edge or corner resizes it. The
   window is **not** re-rendered during the drag: the compositor draws a
   wireframe outline of the prospective rectangle (the same outline style as
   `anim.rs`), and the new size is applied once, on release.
3. **Maximize / restore.** A maximize button in the title bar (and a
   double-click on the title bar) toggles between the window's normal
   rectangle and the work area. The transition uses the existing wireframe
   zoom animation (`Compositor::zoom` in `anim.rs`), like minimize/restore.

## Non-goals

- Live re-rendering while the pointer drags an edge.
- Cursor shape changes over edges (there is one cursor sprite today). A
  follow-up can add resize cursors.
- Snapping/tiling, keyboard-driven move/resize, app-requested maximize.
- Multi-monitor or screen-mode changes.

## Current state (what the code does today)

- `Surface` (`surface.rs`) holds `x, y` (window top-left) and `w, h` (content
  size). `window()`, `title_bar()`, `content()`, `close_button()` and
  `minimize_button()` derive the chrome from them with `BORDER`/`TITLE_H`.
- `event.rs::move_dragged_window` clamps the dragged window **wholly** on
  screen above the taskbar: `x in [0, screen_w - window.w]`,
  `y in [0, screen_h - TASKBAR_H - window.h]`.
- `layout.rs::clamp_on_screen` (placement) already allows a too-big window to
  keep only `visible` pixels on screen.
- Buffers: `present.rs::try_attach` requires a buffer of at least
  `surface.w * surface.h * 4` bytes and records `Mapping { va, bytes, handle }`
  per slot; `render.rs::draw_surface` blits `surface.pixels` using
  `surface.w`/`surface.h` as the buffer's dimensions.
- `anim.rs` has `zoom(from, to)` (private), `outline()` and `lerp()`; minimize
  hides the window via `minimized = true` while the wireframe flies.
- The protocol has no way to tell a client its size changed.

Changing `w`/`h` of a live surface today would make `draw_surface` read the
old buffer with the new stride — an out-of-bounds read. **The buffer's
dimensions must be decoupled from the window's content size** before any
resize lands (step 1 below).

## Design

### 1. Buffer dimensions travel with the mapping

- Add `width: i32, height: i32` to `present::Mapping`, filled from the
  surface's configured size at attach time (the same values `expected` is
  computed from).
- Add `buf_w`/`buf_h` to `Surface` (mirroring the current slot, set in
  `sync_pixels` exactly like `pixels`/`bytes`).
- `draw_surface` blits with `buf_w`/`buf_h` as the source dimensions, into
  `content()` intersected with `clip`, so the blit covers
  `min(buf, content)`. The uncovered part of the content (window grew, client
  has not attached a new buffer yet) is filled with `WINDOW_BG`. A buffer
  larger than the content (window shrank) is cropped. Never read past
  `bytes`: assert `buf_w * buf_h * 4 <= bytes` when building the slice.
- `present`'s `clip_damage` must clip to the *buffer* dimensions of the
  presented slot and then to the content rectangle.
- `try_attach` keeps requiring a buffer sized for the surface's **current**
  configured `w`/`h`. A client that attaches an old-size buffer after a
  resize gets `EINVAL` and must process its pending `Configure` first.

This step alone is behaviour-neutral and should land first (own commit).

### 2. Protocol additions (`idl/display.midl`, append-only)

All via MIDL + `midlc` regeneration — no hand-written constants
(AGENTS.md). Method ids continue from 31 (`PointerWheel`, added on main meanwhile):

```
/// Declare `surface` resizable within these content-size bounds (pixels).
/// Only the creator may call it (`EACCES`; `ENOENT` for unknown). `max_*` of
/// 0 means "screen size". Bounds are clamped to [MIN_CONTENT, screen]; a
/// min > max is `EINVAL`. Until called, a window is fixed-size: no resize
/// edges and no maximize button, so old clients are unaffected.
method SetSizeHints(surface: U64, min_w: U32, min_h: U32, max_w: U32, max_h: U32) -> () = 32;

/// Event: the window manager changed the surface's content size to
/// `width` x `height` (`state` is a `WindowState`). The client should attach
/// buffer(s) of the new size (AttachBuffer, or AttachBufferSlot on non-current
/// slots for Present users) and present a full frame. Until it does, the
/// compositor shows the old buffer cropped/padded.
method Configure(surface: U64, width: U32, height: U32, state: U32) -> () = 33 oneway;

enum WindowState { Normal, Maximized }
```

- Append `Resized`, `Maximized`, `Unmaximized` at the end of `Change` so
  existing values stay stable. Do not reuse `Restored`: it means
  un-minimize.
- Add `maximized: Bool` to `SurfaceRow` (and, if MIDL's field rules allow an
  appended optional parameter on a oneway method, to `SurfaceChanged`; read
  `docs/midl.md` for the compatibility rules and follow them). Update the
  interface doc comment's method list.
- Regenerate the bindings and extend `libs/generated/tests/display.rs`
  round-trip tests for the new methods/structs.

### 3. Compositor model

`Surface` gains:

```rust
pub(super) hints: Option<SizeHints>,      // None = fixed size
pub(super) maximized: Option<Rect>,       // Some(restore window rect) while maximized
pub(super) buf_w: i32, pub(super) buf_h: i32,
```

and helpers `resizable()`, `maximize_button()` (between minimize and close;
minimize moves one slot left), `resize_edges(point) -> Edges`.

New modules (keep every file < 500 lines; `event.rs` must not grow much —
route into the new modules):

- `geometry.rs` — **pure functions**, no compositor state, so they can be
  self-tested:
  - `Edges` bitset (`LEFT|RIGHT|TOP|BOTTOM`).
  - `hit_edges(window: Rect, point, grip) -> Edges`: grip is `RESIZE_GRIP`
    (6 px) around the frame, from 2 px outside the window to 4 px inside;
    corners use a `CORNER_GRIP` (14 px) span so diagonal resize is easy to
    hit. Top edge is only the 4 px above the title bar text area (the title
    bar itself must remain a move handle and its buttons clickable).
  - `resize_rect(start: Rect, edges, dx, dy, min, max) -> Rect`: moves only
    the grabbed edges, clamps content size to hints (convert window ↔
    content with `BORDER`/`TITLE_H`), keeps the opposite edge fixed when a
    clamp kicks in.
  - `keep_reachable(window: Rect, work: Rect) -> (x, y)`: the off-screen rule
    (below).
  - `maximized_rect(work: Rect) -> Rect`.
  - `selftest_geometry() -> &'static str` printing `XUID:GEOM:PASS|FAIL`,
    wired in beside `selftest_titles` / `selftest_focus_on_create`.
- `resize.rs` — the interactive resize: `ResizeDrag { id, edges, start:
  Rect, grab: (i32, i32), outline: Rect }` stored in
  `Compositor::resize: Option<ResizeDrag>`; `begin_resize`, `resize_move`,
  `finish_resize`, `cancel_resize`.
- `maximize.rs` — `toggle_maximize(id)`, `maximize(id)`, `unmaximize(id)`,
  `reflow_maximized()` (re-fit maximized windows when the work area changes,
  e.g. a `"shell"` subscriber hides/shows the fallback taskbar).

Constants go in `theme.rs`: `RESIZE_GRIP`, `CORNER_GRIP`, `MIN_CONTENT_W`
(enough for the three buttons plus some title: ~120), `MIN_CONTENT_H` (~40),
`TITLE_REACHABLE_W` (~64), `DOUBLE_CLICK_TICKS` (50 ticks = 500 ms at the
10 ms PIT), `DOUBLE_CLICK_SLOP` (4 px).

### 4. Off-screen movement

Replace the clamp in `move_dragged_window` with `geometry::keep_reachable`,
where `work` is the work area (`GetWorkArea` semantics: screen minus the
fallback taskbar when it is visible):

- `x` may range so that at least `TITLE_REACHABLE_W` pixels of the title bar
  stay on screen horizontally: `x in [work.x - window.w + TITLE_REACHABLE_W,
  work.x + work.w - TITLE_REACHABLE_W]`.
- `y` may range from `work.y` (the title bar never goes above the top edge,
  otherwise it cannot be grabbed) to `work.y + work.h - TITLE_H` (the title
  bar stays above the taskbar). The body may extend below the screen / under
  the taskbar.

Audit everything that assumed windows are on screen, and fix what breaks:

- `Canvas::fill`/`blit`/`text_face` (`user/src/messenger/display/`) with a
  negative or beyond-screen destination: they must clip source offsets
  correctly (a blit whose `content.x < 0` must skip the first `-content.x`
  source columns, not shift the image). Add a check if missing.
- `Compositor::repaint`/`compose`: damage must be intersected with the screen
  (`full()`) before use; `region.rs` must tolerate covers partly off screen.
- `present`: damage rectangles translated to screen space can be partly off
  screen — intersect with `full()`.
- Hit tests (`contains`, `relative`) already work with any coordinates;
  pointer events outside the screen do not happen.
- `anim.rs::phases`: the icon-sized "small" rectangle centred on a mostly
  off-screen window may be off screen; clamp it into the screen so the
  minimize animation stays visible.
- `origin.rs` (open hints) already clamps; `layout.rs::place_window` is for
  new windows and stays fully on screen.
- Title text and buttons: `close_button()` etc. are derived from `x`, fine.

### 5. Interactive resize (outline only)

- **Press** (`event.rs::pointer_down`): before the title-bar test, if the
  topmost window under the pointer (use a slightly inflated window rect so
  the outer grip counts) is resizable, not maximized, and
  `hit_edges != empty`, consume the press (`consumed |= bit`), raise/focus it,
  and start `ResizeDrag` with `outline = window`. Content presses in the inner
  grip belong to the resize, not the app.
- **Move** (`pointer_move`): while `resize` is set, compute
  `resize_rect(start, edges, pointer - grab, ...)`; damage = old outline ∪ new
  outline (+1 px slack, ∩ screen); `compose(damage)`, draw the new outline
  with `anim::outline` (make it `pub(super)`), present. The window itself is
  composed unchanged at its old geometry — no client traffic during the drag.
- **Release** (`pointer_up`): if the rectangle changed, set `x, y, w, h` from
  it, send `Configure(width, height, Normal)` to the surface's event endpoint,
  `notify_surface(id, CHANGE_RESIZED)`, `repaint_full()`.
- **Escape** (`keys.rs`) cancels the resize (repaint the outline away). A
  surface destroyed mid-resize clears `resize` (mirror the `drag` handling in
  `window.rs` destroy path). Resize and title-bar drag are mutually
  exclusive, and neither may start during a drag & drop session.

### 6. Maximize

- **Chrome:** a maximize button (`icons::draw_maximize`: hollow square;
  `icons::draw_restore`: two offset squares) drawn only for resizable
  windows. `render.rs::draw_surface`'s `reserved` title width accounts for
  the third button.
- **Triggers:** left click on the maximize button; double-click on the title
  bar (two presses on the same window's title bar within
  `DOUBLE_CLICK_TICKS` and `DOUBLE_CLICK_SLOP`; store
  `last_title_click: Option<(id, tick, point)>` on the compositor; the second
  press toggles and does **not** start a drag). Non-resizable windows ignore
  both.
- **Maximize:** `restore = window()`; `target = maximized_rect(work_area)`;
  hide the window (`set_minimized(id, true)`, as `iconify` does), run
  `zoom(window, target)`, apply `x, y, w, h` from `target`, un-hide,
  `Configure(w, h, Maximized)`, `notify_surface(id, CHANGE_MAXIMIZED)`,
  `repaint_full()`. Make `zoom` `pub(super)` (or add
  `Compositor::zoom_between(id, from, to)` next to `iconify`).
- **Restore:** the reverse: zoom from the maximized rect to the saved restore
  rect, apply it, `Configure(.., Normal)`, `CHANGE_UNMAXIMIZED`.
- **While maximized:** no edge resize; a title-bar drag does not move the
  window (optional stretch: drag-to-restore). Minimize/restore from the
  taskbar keeps the maximized state (the iconify/deiconify phases use
  `window()`, so they already animate from/to the maximized rect).
- **Work-area changes:** when the shell subscribes/unsubscribes (taskbar
  visibility changes), call `reflow_maximized()` to re-fit maximized windows
  and send them `Configure`.
- `ListSurfaces` rows report `maximized`.

### 7. Clients

- `user/src/messenger/display/client.rs`: `set_size_hints(...)`, and an event
  decoder for `Configure` in whatever event helper the Rust clients use.
- `xui-app`:
  - `display.rs`: `set_size_hints` call and `Configure` in `decode_event`.
  - `client_window.rs`: `ClientWindow::reconfigure(client, width, height)`:
    allocate a new shared buffer of `width*height*4`, attach it, then close
    the old buffer handle (never before the attach succeeded), update
    `buffer`, `va`, `size`, `rect`. On `EINVAL` (a newer Configure raced)
    keep the old buffer and wait for the next event.
  - `backend.rs`/`input.rs`: on `Configure`, reconfigure the window, update
    the backend `Window`'s `width`/`height`, tell `xui-core` the window was
    resized (find the resize/relayout hook in `xui-core` rev `35c818f`; if
    there is none, re-run layout with the new size and invalidate the whole
    window), then paint and commit a full frame.
  - Opt in: Paint, Files (explorer) and Editor windows call
    `set_size_hints` after `CreateSurface` with sensible minimums. Keep
    fixed-size demos (`xdemo`, `dragdemo`, `shellprobe`) fixed unless trivial.
- Present-pipeline clients (`AttachBufferSlot`/`Present`): on `Configure`,
  attach new-size buffers to non-current slots first, present one, then
  replace the remaining slots as their `BufferRelease` arrives. Document this
  in the MIDL comment; implement only if an in-tree client uses the pipeline.

### 8. Docs

- `docs/architecture/display.md`: window-management section (edges, outline
  resize, maximize, off-screen rule, buffer-size decoupling, `Configure`
  flow).
- `xuid.rs` module header: update the window-management bullet list (the
  "clamped to the screen" sentence is no longer true).
- MIDL doc comments for every new method/enum value.

## Verification

1. `cargo test` for `libs/generated` (display round trips) and `surfbuf` if
   touched; the `XUID:GEOM:PASS` self-test line on boot alongside the
   existing `XUID:*` lines.
2. Build the image and boot it (`python tools/run_demo.py --headless` or the
   screenshot tools).
3. Add `tools/screenshot/examples/window_resize.json` (a `qemu_session.py`
   script) that, with a resizable app (Paint or Files) open:
   - drags the title bar so the window is half off the left edge, then half
     off the bottom; shot;
   - presses on the bottom-right corner, moves by (+150, +100) without
     releasing; shot (outline visible, window unchanged); releases; shot
     (window larger, app re-rendered at the new size — no "Waiting for
     buffer" or garbage);
   - clicks maximize; shots at ~50 ms and after (outline zoom, then a
     work-area-sized window); double-clicks the title bar; shot (restored to
     the previous rectangle).
   Run with `python tools/screenshot/qemu_session.py --image target/lazyos.img
   --out shots/resize --script tools/screenshot/examples/window_resize.json`,
   **Read each PNG** and check it visually, then
   `python tools/screenshot/pngstats.py shots/resize/*.png --min-nonblack 0.01`.
4. Regression: `tools/screenshot/examples/multitask_demo.json` still looks
   right; minimize/restore animations still work; fixed-size windows show no
   maximize button and no resize edges.
5. No kernel code changes are expected, so the kernel suite is not required;
   if any kernel file is touched, `python tools/test/run.py --accel none` must
   pass.

## Suggested commit order

1. Buffer dimensions per mapping (behaviour-neutral).
2. `geometry.rs` + self-test; off-screen movement + clipping audit.
3. MIDL additions + regenerated bindings + client-library helpers.
4. Interactive outline resize + `Configure`.
5. Maximize button, double-click, zoom animation, `reflow_maximized`.
6. xui-app `Configure` handling and opt-in for Paint/Files/Editor.
7. Docs + session script.

## Risks

- **Stride mismatch reads** if any path still assumes `buffer == content`
  size — step 1 must cover `draw_surface`, `present` damage clipping and any
  other `surface.w/h` use on pixel data (grep for `surface.pixels`).
- **Races** between `Configure` and a client's in-flight attach/present: the
  per-mapping dimensions make any mix safe to draw; strict attach sizing
  makes a stale attach fail cleanly.
- **Blocking animation**: `zoom` blocks the compositor ~120 ms, as minimize
  already does; acceptable.
- **File size**: `event.rs`, `render.rs` (396 lines) and `surface.rs` must stay
  under 500 lines — put new logic in the new modules.

## Implementation notes (what turned out different)

- **`Configure` decoding.** `xui-app/src/display.rs` gained a
  `Event::Configure` variant handled in the backend's `route_client_event`;
  the native `user::messenger::display` client only gained `set_size_hints`
  (the in-tree native demos are fixed-size and their `Event` match stays
  exhaustive).
- **App opt-in.** Rather than each app threading a window id to
  `SetSizeHints`, `LazyOSBackend::set_size_hints(min_w, min_h, max_w, max_h)`
  records the bounds before `run_app` and `open_window` applies them to every
  window the app opens. Paint, Files and Editor call it; the fixed-size demos
  do not.
- **Off-screen movement is general.** `keep_reachable` replaced the clamp for
  every window, not only resizable ones; fixed-size clients keep their exact
  chrome (two buttons, no edges) but can now be moved partly off screen.
- **`SurfaceChanged` carries `maximized`** as an appended parameter (field id
  11), and `SurfaceRow` gained the matching field; the `Change` enum appended
  `Resized`/`Maximized`/`Unmaximized` (7/8/9).
- **`present` damage** is clipped to `min(buffer, content)` of the presented
  slot, and `Compositor::repaint`/`compose` intersect damage with the screen,
  which also covers the partly off-screen case.
- **Attach size stays "at least".** `try_attach` still accepts a buffer of
  at least `width * height * 4` bytes, so existing clients are unaffected. The
  mapping records the size it was attached for and the renderer uses that as
  the stride, so reads never go out of bounds. A stale *larger* buffer
  attached while a shrink is in flight is drawn at the new stride (one wrong
  frame) until the client handles its `Configure` and attaches again; a stale
  smaller one is refused with `EINVAL`.
- **`zoom`/`outline`** were made `pub(super)` rather than adding a wrapper.
