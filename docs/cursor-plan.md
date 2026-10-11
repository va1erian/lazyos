# Cursor plan: antialiased, resizable, HiDPI cursors with a shape API

Status: C1-C8 landed on `claude/high-dpi-cursor-api-aeff76`, except the xui revision bump (see Status). Owner branch: `claude/high-dpi-cursor-api-aeff76`.

## Goal

1. Keep today's arrow silhouette (it is liked) but draw it **antialiased** and
   at a **configurable size**, sharp at every HiDPI scale (no block doubling).
2. Add cursor **shapes**: Default (the arrow), Busy, Resize (horizontal,
   vertical, NW-SE, NE-SW) and Hand (hyperlink). Text (I-beam) is a cheap
   extra because xui widgets already ask for it.
3. A Messenger **API** (`SetCursor`) any *authorized* app can call for its own
   windows; xui apps get it for free through `Backend::set_cursor`.
4. **LazyWeb** shows the hand over links (NetSurf's `set_pointer`).
5. **Settings** gets a Cursor group: size (and a preview).
6. The compositor itself shows resize cursors over window borders.

## Today (research summary)

- xuid draws the cursor in software: `user/src/messenger/display/font5x7.rs:13`
  (8x8 1-bit `CURSOR`), raster `canvas.rs:405-490` (`cursor_scaled`,
  `cursor_masks`: body + dilated outline, opaque, nearest-neighbour `scale`).
- Overlay with save-under: `user/src/bin/xuid/cursor.rs` (fixed
  `SAVE_BYTES = 11*4*11*4*4`, `lift`/`stamp`/`move_cursor`, self-test
  `XUID:CURSOR:PASS` at 142-185). Sprite rect: `layout.rs:79-84`
  (`cursor_rect`, 11*scale square, hotspot implicit at `(scale, scale)`), also
  used by `drag.rs`. Lift/stamp callers: `render.rs:42-56`, `resize.rs:79-84`,
  `anim.rs:107-129`, `event.rs:70`.
- Kernel `kernel/src/cursor.rs` is only the mux fallback (no compositor). Out
  of scope, except fixing its inverted doc comment if touched.
- `idl/display.midl`: next free method id is **44**. No cursor method.
- xui (`va1erian/xui`, pinned `c0007e4…` at planning time, now `93592fd`, local clone `E:\repos\xui`) already
  has `xui_core::backend::Cursor { Default, Hand, Text, SizeHorizontal,
  SizeVertical, SizeNwSe, SizeNeSw }` and `Backend::set_cursor(id, cursor)`;
  widgets (hyperlink, edit, split, litehtml links…) call it. **LazyOSBackend
  does not implement it** (`xui-app/src/backend/handlers.rs`). No Busy variant.
- NetSurf glue (`xui-netsurf/src/sys/host.rs:142-147`, `host_win_pointer`) is a
  no-op upstream.
- Package permissions are per interface (`libs/pkgstore/src/rules.rs`): every
  desktop app already holds all of `os.lazy.display.v1`. Per-method gating uses
  a **pseudo-interface scope** like `os.lazy.clipboard.write.v1`
  (`user/src/messenger/clipboard/mod.rs:39-48`, `xui-app/src/platform/clipboard.rs`,
  `rules.rs:103-113`, consent text in `libs/pkgstore/src/explain.rs`).
- Settings: `xui-app/crates/settings/src/` (`sections.rs`, `app.rs` 453 lines,
  `appearance_page.rs`, `theme_ops.rs`). xuid follows `sys/ui/#` in
  `user/src/bin/xuid/themefeed.rs` (`reload` reads `uitheme::ALL_KEYS`).

## Design

### D1. `libs/cursorgfx` — a pure, host-tested sprite rasteriser

New `no_std + alloc` crate (no floats needed; fixed-point like
`xuid/icons.rs`), used by xuid and testable with `cargo test -p cursorgfx`.

- `pub enum Shape { Default, Busy, Text, Hand, ResizeH, ResizeV, ResizeNwse, ResizeNesw }`
  with `from_wire(u32) -> Option<Shape>` matching the MIDL enum (D3).
- Each shape is **vector outlines in a 32x32 design grid** (polygons; the arrow
  is the current 8x8 bitmap's silhouette traced as a polygon and scaled up, so
  it reads as the same cursor). Fill light (`240,240,240`), outline dark
  (`0,0,0`), outline width ~1/16 of the size (at least 1 physical px), plus an
  optional soft 1 px shadow (alpha ~25%) for depth.
- `pub fn render(shape, px: u32, frame: u32) -> Sprite` where
  `Sprite { w, h, hot_x, hot_y, pixels: Vec<u32 /* premultiplied ARGB */> }`.
  Coverage by scanline polygon fill with 4x4 (or 8x8) subsamples per pixel,
  non-zero winding. Hotspots: arrow tip; hand index fingertip; resize and
  I-beam and busy at the centre.
- Busy: the arrow plus a small ring spinner at its lower right, `BUSY_FRAMES`
  (8) rotation frames (`frame % 8`).
- `pub fn max_extent(px) -> u32`: largest sprite bound for a size (for the
  save-under buffer).
- Tests: every shape at every size in `SIZES` x scale 1..=4 renders non-empty,
  in bounds, hotspot inside; edge pixels have partial alpha (proves AA); the
  arrow at size 16 covers the old bitmap's pixels (silhouette kept);
  deterministic output (golden checksum per shape at one size); busy frames
  differ.

### D2. Size configuration (`libs/uitheme/src/cursor.rs`)

New module like `scale.rs` (lib.rs is 473 lines: do not grow it beyond the
`mod`/`pub use` lines).

- `KEY_CURSOR_SIZE = "sys/ui/cursor/size"`: design pixels, one of
  `16 | 24 | 32 | 48` (Small, Medium, Large, Extra large). Default **16**
  (close to today's 10 px sprite; a few px bigger reads better once AA).
  Parse tolerant: unknown → default.
- Physical size = `design * scale` (xuid's integer HiDPI scale), so a 2x
  desktop gets a real 32 px rendering, not doubled pixels.
- Not in `ALL_KEYS` unless reset should clear it (it should: add it to the
  reset list Settings uses, check `theme_ops.rs::reset`). If `ALL_KEYS` drives
  the xuid palette reload, keep cursor keys in their own list
  (`CURSOR_KEYS`) and handle them separately (D4).

### D3. Wire: `SetCursor` in `idl/display.midl`

```
enum CursorShape { Default = 0, Busy = 1, Text = 2, Hand = 3,
                   ResizeH = 4, ResizeV = 5, ResizeNwse = 6, ResizeNesw = 7 }
/// Sets the cursor shown while the pointer is over this surface's content.
/// Caller must own the surface and carry the cursor scope (see below).
SetCursor(surface: u32, shape: u32) -> () = 44
```

Append-only id 44. Regenerate everything with the CI command:

```
python tools/midlc/midlc.py --out libs/generated/src/lib.rs --manifest idl/manifest.json --docs docs/idl --schema libs/rhai-lazy/src/msg/idl.rs --rhai-api libs/rhai-lazy/api idl/*.midl
```

Round-trip tests go in a **new** file `libs/generated/tests/display_cursor.rs`
(`display.rs` is allowlisted at 508 lines).

### D4. Authorization: the `os.lazy.display.cursor.v1` scope

"Authorized apps" = holders of a new pseudo-interface scope, mirroring the
clipboard write scope exactly:

- The client sends `SetCursor` with `fnv1a64("os.lazy.display.cursor.v1")` as
  the parcel's interface id; the kernel's label rules decide before xuid sees
  it (`LABEL:DENY` under `LAZYOS_LABEL_TRACE=1`).
- `libs/pkgstore`: make it a declarable manifest interface (resolves to the
  `display` service name like display.v1), add consent text in `explain.rs`
  ("Change the mouse pointer over its own windows"). It is **not** implied by
  display.v1. Tests in `rules.rs`/`explain.rs` (split a test module out if a
  file would pass 500 lines).
- Add it to the manifests of the core xui packages
  (`xui-app/packages/*/manifest.toml`, LazyRAD's, Doom's if it has windows)
  — all of them use xui widgets that request cursors. Unlabelled tasks
  (Terminal, LazyShell) pass by uid rules as today.
- xuid: `carries_declared`/`handle_request` accept that interface id **only**
  for `METHOD_SET_CURSOR`; any other method under it → empty/EINVAL reply.
  Then the `SetTitle` checks: surface exists (`ENOENT`), `owner ==
  message.sender` (`EACCES`), shape decodes (`EINVAL`).
- Rhai: `display.rhai` regenerates; LazyRAD forms can call it if their package
  declares the scope.

### D5. xuid: sprite cache, AA blit, shape resolution

- `user/src/bin/xuid/cursor.rs`: replace the fixed `SAVE_BYTES` array with a
  `Vec<u32>` sized from `cursorgfx::max_extent(size*scale)`; reallocate on size
  change. Cache rendered sprites per shape (`[Option<Sprite>; 8]` plus busy
  frames), invalidated when size or scale changes. Move any helper that would
  push a file past 500 lines into `xuid/cursor_shape.rs`.
- Blit: new alpha-over blit of premultiplied ARGB onto the framebuffer
  (`blend_pixel`/`blend_span` in `canvas.rs` are the model; canvas.rs is 493
  lines, so **move** `cursor`/`cursor_scaled`/`cursor_masks` out into a new
  `display/cursor_blit.rs` and delete the old 1-bit sprite and `CURSOR`
  constant once nothing uses them).
- `layout.rs::cursor_rect(point, …)` takes the current sprite's
  `w, h, hot_x, hot_y`: `Rect::new(x - hot_x, y - hot_y, w, h)`. Update
  `drag.rs` callers to use the overlay's current rect (store it on the
  overlay) rather than recomputing.
- Shape changes go through **lift → swap sprite → stamp** and present the union
  of old and new rects (same cheap path as `move_cursor`).
- `Surface` gets `cursor: Shape` (default `Default`).
- Effective shape, recomputed on every pointer move and on `SetCursor`
  (`Compositor::effective_cursor`):
  1. Window resize or move in progress → the resize shape of the dragged edges
     (`geometry::Edges` → ResizeH/V/Nwse/Nesw); move → Default.
  2. Pointer on a resizable window's resize border (`hit_edges`, call it on
     move too, not only on press) → that resize shape.
  3. Pointer over a surface's content (the `wheel_target` hit-test order:
     panel, topmost window content, desktop) → that surface's `cursor`.
  4. Otherwise Default. Shutdown overlay keeps hiding the cursor.
- Busy animation: while the effective shape is Busy, advance the frame every
  ~100 ms from xuid's existing tick/deadline machinery (see `anim.rs` /
  the main loop's poll timeout); no timer when not busy.
- `themefeed.rs`: read `KEY_CURSOR_SIZE` at start and in `reload`; a cursor-size
  change re-renders the cache and re-stamps the cursor, **without** the full
  palette repaint. (HiDPI scale stays fixed per compositor lifetime, as today.)
- Self-test `selftest_cursor`: keep the marker `XUID:CURSOR:PASS`; drop the
  `< 11 px` assumption, assert the stamped rect is the sprite's and that lift
  restores the background exactly, for Default and Hand.
- When a surface is destroyed or its owner dies, nothing special: its shape
  goes with it.

### D6. Clients

- **Native client** `user/src/messenger/display/client.rs`: `set_cursor(surface, shape)`
  with the scope interface id (427 lines; put it in a small sibling module if
  needed).
- **xui-app** `xui-app/src/display.rs`: `Client::set_cursor` (model:
  `set_title`), parcel interface id = the cursor scope.
- **LazyOSBackend::set_cursor** (`backend.rs` is at its 505-line ceiling: new
  `xui-app/src/backend/cursor.rs`): store `HashMap<WidgetId, Cursor>`; on
  `pointer_move` (and enter/leave, `pointer.rs`), resolve the hit node, then
  its nearest ancestor with a cursor, map to `CursorShape`, and send
  `SetCursor` **only when it differs** from the last shape sent for that
  surface. If xuid/kernel refuses (EACCES/denied), remember it per client and
  stop sending (no log spam). Clear entries on node removal.
  Map: Default→Default, Hand→Hand, Text→Text, SizeHorizontal→ResizeH,
  SizeVertical→ResizeV, SizeNwSe→ResizeNwse, SizeNeSw→ResizeNesw, Busy→Busy.
- **Busy from app code**: `xui_app::cursor::set_busy(window, bool)` helper that
  overrides the per-node shape for the whole window (used while an app does
  long work, e.g. LazyWeb page loading — optional — and app start).
- Host tests in `backend/cursor.rs` with `test_support::client_backend()`:
  hover over a hyperlink node sends Hand once; moving within it sends nothing;
  leaving sends Default; ancestor fallback; refusal stops further sends.

### D7. Upstream xui changes (`E:\repos\xui`, `va1erian/xui`)

Work on a branch in a separate worktree of `E:\repos\xui` (its main checkout
has other work in progress; do not touch it).

1. `xui-core/src/backend/cursor.rs`: add `Cursor::Busy` (keep the enum
   `#[non_exhaustive]`-friendly; update winit backend mapping to
   `CursorIcon::Wait`).
2. `xui-netsurf`: `host_win_pointer(ctx, win, shape)` sends
   `Output::Pointer(shape)` through `WinState`; `NetSurfWidget::drain` maps
   NetSurf `gui_pointer_shape` to `Cursor` (POINT→Hand, CARET→Text,
   WAIT/PROGRESS→Busy, LEFT/RIGHT→SizeHorizontal, UP/DOWN→SizeVertical,
   LU/RD→SizeNwSe, RU/LD→SizeNeSw, else Default) and calls
   `ui.set_cursor(control.id(), cursor)` only on change. Test the mapping.
3. Push the branch and open a PR on `va1erian/xui`; **ask the user before
   merging**. Then bump the pinned rev everywhere it appears (`grep -rn
   8ac63a0a769726f2d66b758cce9092587292e9f7` over `*.toml`, `docs/xui-plan.md`);
   LazyRAD (`lazyrad-os`) pins `va1erian/lazyrad` which pins xui too — keep one
   xui revision in every graph (`cargo tree -d` shows duplicates) or LazyRAD
   must be bumped as well (see #602). Until merged, a temporary `[patch]` is
   acceptable for local verification only, never committed.

### D8. LazyWeb

With D6 + D7 nothing LazyWeb-specific is needed beyond the rev bump: the
`NetSurfView`'s control is the hit node under the pointer. Verify: hovering a
link on the example.com stand-in shows the hand. Add a `WEB:CURSOR:HAND` evidence
line? No — the verdict is the screenshot (QEMU screendumps include the
software cursor). Extend `tools/web/session.py` to hover the link and shoot,
regenerate `lazyweb.json` with `--write`, and teach the judge
(`tools/web/judge`/`test_judge.py`) nothing new unless cheap.

### D9. Settings: Cursor group

- In the Appearance page (`appearance_page.rs`, 117 lines), not a new
  sidebar section (a new section shifts sidebar rows and breaks the
  `xui_settings*.json` click coordinates). Group "Pointer": a size choice
  (Small / Medium / Large / Extra large → 16/24/32/48) and a live preview
  image of the arrow and hand drawn with `cursorgfx` at the chosen size.
- `app.rs` is 453 lines: add `Msg::CursorSize(u32)` but put load/apply logic in
  a new `cursor_ops.rs` (pattern: `theme_ops.rs::set_animations`). Reset
  restores 16.
- Settings package already has confd; `sys/` keys need uid 0 or whatever the
  existing appearance keys use — reuse the same write path.
- Tests: `crates/settings/tests/window.rs` snapshot still renders; a new test
  that choosing Large writes `sys/ui/cursor/size=32` to the fake store and
  reset clears it.

### D10. Docs

- This file → status per phase as it lands.
- `docs/hidpi-plan.md`: cursor row now "AA vector sprite rendered at
  size x scale".
- `docs/architecture/display.md`: overlay section (sprite cache, hotspot,
  shape resolution order, busy tick).
- `docs/window-resize-plan.md`: resize cursors done.
- `docs/packages.md` / consent list: the new scope.
- `docs/settings-app-plan.md`: the new key.
- AGENTS.md: one short paragraph under HiDPI pointing here.

## Phases (each a commit, each verified)

| # | What | Verify |
|---|------|--------|
| C1 | `libs/cursorgfx` + tests | `cargo test -p cursorgfx` |
| C2 | xuid draws AA Default arrow from cursorgfx, dynamic save-under, size key | build desktop, `cursor_fast.json`, `cursor_hidpi.json` sessions; zoom-crop the PNG around the pointer and Read it: smooth edges, same silhouette, sharp at 2x |
| C3 | Compositor-native resize cursors over borders and during resize | `window_resize.json` + a new `cursor_shapes.json` session hovering each border |
| C4 | MIDL `SetCursor` + scope + xuid handler + pkgstore + manifests | `cargo test -p messenger-generated -p pkgstore`, midlc CI command clean diff, `core_apps.json` under `LAZYOS_LABEL_TRACE=1` shows no `LABEL:DENY` |
| C5 | xui-app backend `set_cursor` + busy helper + host tests | `cargo test --manifest-path xui-app/Cargo.toml` (lib tests via WSL/musl per AGENTS.md); session hovering a hyperlink/edit in an app (e.g. Settings or Docs) shows Hand/I-beam |
| C6 | xui upstream (Busy, NetSurf pointer) + rev bump | xui tests; `python tools/web/run.py` and a screenshot with the hand over a link |
| C7 | Settings Pointer group | settings crate tests; `xui_settings.json`-style session changing size, screenshot shows a bigger cursor live |
| C8 | Docs | — |

Global checks before finishing: `python tools/check_file_length.py`,
`python tools/fhs/check_literals.py`, `python tools/test/run.py --accel none`
(kernel untouched, but the suite must still pass), HiDPI build
(`LAZYOS_DISPLAY_MODE=2560x1440`) screenshots of every shape at 1x and 2x.

## Non-goals

Custom app-supplied bitmaps (only the named shapes), animated cursors other
than Busy, hardware cursor planes, fractional scales, live HiDPI scale change,
the kernel mux cursor.

## Status and deviations

| # | State | Evidence |
|---|-------|----------|
| C1 | done | `cargo test -p cursorgfx`; `cargo run -p cursorgfx --example dump -- 64 sheet.ppm` draws every shape |
| C2 | done | `cursor_fast.json`, `cursor_hidpi.json` (2x: a 32 px antialiased arrow) |
| C3 | done | `cursor_shapes.json` (every border and corner, a resize in progress), `window_resize.json` |
| C4 | done | `core_apps.json` under `LAZYOS_LABEL_TRACE=1`: no `LABEL:DENY`; `cargo test -p messenger-generated -p pkgstore` |
| C5 | done | `cursor_docs.json` (hand over a link, I-beam over text, arrow over a button); `cursor_xdemo.json` (all eight shapes through `SetCursor`, busy animating); host tests in `xui-app/src/backend/cursor/tests.rs` |
| C6 | done | `va1erian/xui#279` (`Cursor::Busy`, NetSurf pointer shapes) is in the pinned xui (`93592fd`, which contains `8ac63a0`); `wire_shape` maps `Cursor::Busy`. `python tools/web/run.py` passes and `shot_hover_link.png` shows the hand |
| C7 | not landed | The Mouse pointer group on the Appearance page and the `cursor_size.json` session. They were written against `sys/ui/*` keys that Settings edited directly; Appearance is now each account's own `user/<uid>/ui/*` copy (`user_theme`), so the size key needs a `user_key` mapping and a Make-default path first. Until then the size is `sys/ui/cursor/size` set by hand (`confd`) |
| C8 | done | this file, `docs/architecture/display.md`, `docs/hidpi-plan.md`, `docs/window-resize-plan.md`, `docs/packages.md`, `docs/settings-app-plan.md`, `docs/lazyweb.md`, `AGENTS.md` |

Deviations from the design above:

- **D3/D4: `SetCursor` is its own interface**, `os.lazy.display.cursor.v1`
  (`idl/display_cursor.midl`, method 1), not method 44 of `display.v1`:
  `display.midl` is at its allowlisted length, and the scope the kernel checks
  is an interface id, so one interface is both the wire and the scope with no
  hand-written constant. It is served on the display endpoint and also
  registered under its own name (the generated Rhai module connects by name).
  `pkgstore` knows it as an ordinary (declared, not implied) interface.
- **D1: the arrow is the old silhouette** (a right triangle with a short tail,
  traced from the 8x8 bitmap) but drawn light with a dark outside outline, per
  the plan's colours, not the old white-ring-on-black look. No soft shadow.
- **D5: the busy tick** is the main loop's existing 100 ms idle wake; no timer.
- **D6**: LazyRAD's packaged players (`lazyrad-os`) do not list the scope; the
  IDE package does. A form app that wants shapes adds it to its manifest.
- **Limit**: a window only gets pointer moves while it is focused (`xuid`
  forwards moves to the focused surface), so shapes over an unfocused window's
  content appear after it is focused, like hover highlights.
- `pkgstore`'s explanation table lacked `os.lazy.messenger.topics.bell.v1`
  (a pre-existing failing test once `idl/manifest.json` listed it): added.
