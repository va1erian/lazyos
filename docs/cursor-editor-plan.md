# Cursor Studio plan: draw a vector cursor, then have xuid use it

Status: V1-V5 built (see "Status and deviations" at the end). Builds on [`cursor-plan.md`](cursor-plan.md) (C1-C8:
`libs/cursorgfx`, `SetCursor`, the size key).

## Goal

1. A desktop app, **Cursor Studio**, to draw the eight pointer shapes as
   vector polygons, with a live preview at the real sizes and HiDPI scales.
2. One click **applies** the result: `xuid` redraws its pointer from the new
   shapes at once, no reboot, no scene repaint.
3. The drawing survives reboots and can be saved, exported and imported as a
   small text file ("cursor theme").

## Today (what we reuse)

- `libs/cursorgfx` already is a vector cursor renderer: every `Shape` is a
  list of polygons on a 32x32 design grid (`shapes.rs`, half-cell points,
  design unit 1/16 cell), filled light with a dark outline, 4x4 subsampled,
  non-zero winding (`raster.rs`), premultiplied ARGB `Sprite` with a hotspot.
  The shapes are **hard-coded tables** today; `rotated()` computes the NW-SE
  and NE-SW arrows from the horizontal one.
- `xuid` caches one sprite per shape plus the busy frames
  (`user/src/bin/xuid/cursor.rs`), picks the shape in
  `cursor_shape.rs::shape_at`, and follows `sys/ui/cursor/size` through
  `themefeed.rs` -> `apply_cursor_size` (lift, swap, stamp, present two small
  rects). That path is exactly what a theme change needs.
- Settings already writes confd keys; `libs/uitheme/src/cursor.rs` owns the
  key and its tolerant parser.
- No new Messenger interface is needed (confd carries the key), so nothing
  goes in `devtool::DENY` or `reviewed_interfaces.txt`.

## Design

### V1. Make shapes data: `cursorgfx::theme`

New module (keep `lib.rs`/`shapes.rs` under 500 lines; the parser goes in its
own file).

- `ShapeDef { hot: (u16,u16), layers: Vec<LayerDef> }`,
  `LayerDef { polys: Vec<Vec<(i16,i16)>>, fill: u32 /* 0xRRGGBB */, outline: u32, thin: bool }`
  in design units (the editor snaps to half cells = a 64x64 grid, matching the
  tables). `Theme { shapes: [Option<ShapeDef>; SHAPE_COUNT], name }`; a `None`
  slot falls back to the built-in shape, so a theme may restyle only the arrow.
- `render_themed(theme, shape, px, frame)` builds the existing `Layer`s from
  the definition and reuses `raster::draw_layer`. `Layer` grows an RGB fill
  and outline colour (today: grey fill, black outline); built-in shapes keep
  grey 240 / black, so their pixels do not change (golden checksum test).
- **One way to do it:** convert the built-in tables into the same data
  (rotated shapes baked out, the busy spinner stays code-generated and is
  used when a theme gives no Busy) so the default theme is just a `Theme`
  and "Reset" and "Start from built-in" are free. Export the built-ins to a
  theme file once with the existing `examples/dump.rs` style tool.
- **Text format** `.lcur` (line based, diff-friendly, no float, no JSON in the
  compositor): 
  ```
  lazyos-cursor 1
  name Midnight
  shape hand hot 22 4
  layer fill f0f0f0 outline 000000
  poly 22 8  25 4  29 4 ...
  ```
  `parse(&str) -> Result<Theme, Error>` and `serialize(&Theme) -> String`,
  round-trip tested.
- **Untrusted input:** `xuid` parses a file a user wrote, so parsing is
  strict and bounded: coordinates clamped to the design square plus margin,
  generous but finite totals (say 64 layers, 4096 vertices per shape; generous
  on purpose, but a loop in the compositor must end), no recursion, parse
  errors never panic. A fuzz-style host test feeds random and truncated text.
  Any failure means: keep the previous theme, emit a trace line.
- **Not now:** curves in the rasteriser. The editor may offer Bezier handles
  that it flattens to polygons on save, so the format and `raster.rs` stay
  polygon-only.

### V2. Where the theme lives and how xuid learns of it

- Files (decision, see "Decisions"): system themes in
  `<SYSTEM_SHARE>/cursors/<name>.lcur` (`/system/share/cursors`, a new
  `libs/fhs::share` constant; `build.rs` ships the built-in export there),
  user themes in `app_data_dir(home, "os.lazy.cursored")/themes/<name>.lcur`
  (a new `fhs` helper; no literals, `check_literals.py`).
  **First task of V2, before any other code:** confirm which of these `xuid`
  can read (it reads fonts and icons from the OS volume; `/home` may be a
  separate volume mounted after login). If a user dir is unreachable, the
  fallback is storing the theme text in confd under `user/<uid>/ui/cursor/theme`
  (check confd's value size limit); the rest of the design does not change.
- Key `user/<uid>/ui/cursor/theme` with `sys/ui/cursor/theme` as the default =
  theme name (empty = built-in), next to the size key in
  `uitheme::CURSOR_KEYS`, same tolerant parser pattern. This is also where the
  `user_key` mapping for the size key (blocked C7) gets done, once, for both.
- `themefeed.rs` reads the key, loads the file **on its worker path, not the
  render path**, and exposes `take_cursor_theme()`. `Compositor::apply_cursor_theme`
  mirrors `apply_cursor_size`: lift, replace the theme in the overlay (clears
  the sprite cache, resizes save-under from the largest custom extent), stamp
  at the pointer, present old+new rects. Sprites are rendered lazily and
  cached, as today; a custom sprite's extent comes from `max_extent`.
- Evidence: `XUID:CURSOR:THEME:PASS|FAIL` and a `theme=<name>` field on the
  existing cursor self-test, through `user::trace`, never a print. Extend
  `selftest_cursor` to run once with a tiny test theme.

### V3. The app: Cursor Studio (`xui-app/packages/cursored`)

A Rust xui app (custom canvas editor, so not Rhai). Start it with
`python tools/xui/new_app.py cursored --name "Cursor Studio" --description "Draw the mouse pointer"`
which registers the launcher, icons, build switch and core package.

Layout:

- **Left:** the eight shapes (Arrow, Busy, Text, Hand, four resizes) with
  thumbnails; a theme name field.
- **Centre:** the editing canvas, a 64x64 half-cell grid over the 32x32
  design square, zoom and pan, hotspot crosshair, the outline margin shown
  faintly. Tools: Select/move vertex, Add vertex on an edge, New polygon,
  Delete vertex/polygon, Hotspot, Mirror H/V (resizes), Rotate 45 (derive the
  diagonal resizes), plus snap toggle, undo/redo.
- **Right:** layer list (reorder, add, delete), fill and outline colour
  pickers, "thin outline", and a **preview strip** rendered with
  `cursorgfx::render_themed` at 16/24/32/48 and scale 1x/2x on light, dark and
  photo-like backgrounds. Because the preview is the same rasteriser xuid uses,
  what you see is what the compositor draws.
- **Buttons:** New from built-in, Open, Save, Save as, Import/Export `.lcur`,
  **Apply** (save + write `sys/ui/cursor/theme`), **Revert to built-in**.
  File I/O and the confd write run on worker threads; the UI thread only
  draws. The editor never needs the pointer to *be* the new cursor to be
  usable, but the Apply result is visible immediately because the real pointer
  changes under the mouse.
- Core package: manifest permissions derived from a `LAZYOS_LABEL_TRACE=1`
  run (confd write for the cursor key, file access to the theme dirs), listed
  in `tools/xui/core_packages.py`. It declares `os.lazy.display.cursor.v1`
  too, so the canvas can show a crosshair/hand while editing.
- Launch from both front ends: build switch `LAZYOS_CURSORED=1`,
  `run_demo.py --cursored`, GUI control in `tools/lazygui/catalog.py`
  (`build_env`, `build_plan`) with tests in `test_catalog.py`.
- Optional entry in Settings -> Appearance: "Edit pointer..." launching the
  app (do not add a sidebar section; it shifts the session click
  coordinates).

### V4. Tests

- `cargo test -p cursorgfx`: parse/serialize round trip; fuzz/truncation never
  panic; limits reject oversize input; built-in theme renders pixel-identical
  to today's golden checksums; custom theme with a missing slot falls back;
  hotspot inside the sprite; every shape renders non-empty at every size and
  scale; partial-alpha edges (antialiasing) for custom polygons.
- `cargo test -p uitheme`: theme key parser (unknown/empty -> built-in).
- xuid: host tests for `apply_cursor_theme` ordering (lift before swap,
  stamp restores background) where the module is testable; otherwise the
  self-test above.
- App: `cargo test --manifest-path xui-app/Cargo.toml -p cursored` including a
  snapshot test of the window (Read the PNG) and editor operation tests
  (add/move/delete vertex, undo/redo, apply writes the key to a fake store).
- Session: `tools/screenshot/examples/cursor_theme.json` opens the app, loads
  a bundled test theme, clicks Apply, moves the mouse over empty desktop and
  over a link, and takes zoomed shots; the judge is the pixels (Read them) plus
  `XUID:CURSOR:THEME:PASS`. HiDPI run with `LAZYOS_DISPLAY_MODE=2560x1440`.

## Phases (each a commit, each verified)

| # | What | Verify |
|---|------|--------|
| V1 | `cursorgfx` theme data, parser, `render_themed`, RGB layers, built-in export | `cargo test -p cursorgfx`, golden unchanged |
| V2 | Theme key, `fhs` paths, ship built-in theme in the image, `themefeed` + `apply_cursor_theme`, self-test | build desktop, `cursor_fast.json`, set the key by hand (`confd`), zoom-crop the pointer |
| V3 | Cursor Studio read-only: open a theme, preview, Apply | app snapshot, `cursor_theme.json` |
| V4 | Editing tools, undo/redo, save/export/import | editor op tests, session that draws a polygon |
| V5 | Launch wiring (build switch, `run_demo.py`, GUI, core package, permissions from label trace) | `core_apps.json` under `LAZYOS_LABEL_TRACE=1`: no `LABEL:DENY` |
| V6 | Bezier handles flattened on save; mirror/rotate helpers; bundled sample themes | tests + screenshots |
| V7 | Docs: this file, `architecture/display.md`, `packages.md`, `settings-app-plan.md`, a short `AGENTS.md` pointer | `check_snippets.py`, `site/build.py --strict` |

Pre-push basics from `AGENTS.md` apply (fmt, file length, trace ratchet, fhs
literals, clippy for `cursorgfx`/`user`, xui checks). Drawing and compositor
changes need `/code-review high` run twice; attack points: parser bounds,
integer overflow on coordinates and sprite extent, save-under sizing after a
theme swap (a larger custom extent), a pointer drawn twice, damage that misses
the old rect.

## Decisions

1. **Colour:** per-layer RGB fill and outline. The built-ins keep grey 240 and
   black, so their pixels do not change.
2. **Busy:** one static custom shape, or the built-in spinner when the theme
   leaves Busy out. No custom animation (a later `frames` extension to the
   format would not break v1 files, so the `lazyos-cursor 1` header stays).
3. **Scope:** per account (`user/<uid>/ui/cursor/theme`), falling back to the
   system value `sys/ui/cursor/theme`, resolved with the same `user_key` /
   make-default mapping the cursor size needs (C7 in `cursor-plan.md`). xuid
   follows the active session's value. Do this mapping in V2 for both keys.
4. **Files:** extension `.lcur`; system dir `/system/share/cursors`, user dir
   `<home>/.apps/os.lazy.cursored/themes` (V2 above). Theme names are
   `[a-z0-9-]{1,32}`, the file name is the name, so the confd value can never
   carry a path (no traversal; xuid joins it to a fixed directory).

## Risks to settle early

- xuid file access to the user dir (V2 first task).
- Whether `Layer` colour changes ripple into `raster.rs` blending: do V1 as a
  pure refactor commit with the golden checksums unchanged, then add colour.
- Largest custom extent can exceed the built-in one (a polygon may reach the
  margin): `max_extent` must cover a theme's actual bounds, and the save-under
  buffer is resized while lifted, as `set_design_size` does.
- Resize and Hand hotspots a theme sets must be clamped inside the sprite.

## Non-goals

Bitmap or animated cursors, per-app cursor themes, hardware cursor planes,
the kernel mux cursor, live HiDPI scale change.

## Status and deviations

| # | State | Evidence |
|---|-------|----------|
| V1 | done | `cargo test -p cursorgfx` (theme, `.lcur` parser, fuzz, built-in theme pixel-identical) |
| V2 | done | `xuid` builds; `selftest_cursor` runs a custom theme (`XUID:CURSOR:PASS`); `CURSOR:THEME:APPLIED/REFUSED` trace |
| V3-V4 | done | `xui-app/crates/cursored` (model, canvas, preview, store), `cargo test -p xui-cursored`, snapshots `target/snapshots/cursored-*.png` |
| V5 | partly | build switch, `run_demo.py`, GUI entry, core package scaffolded; permissions need a `LAZYOS_LABEL_TRACE=1` run |
| V6 | not done | Bezier handles, mirror/rotate helpers, sample themes |
| V7 | partly | this file |

Deviations from the design:

- **Storage is the confd value, not a file.** The theme text lives in
  `user/<uid>/ui/cursor/theme` (machine default `sys/ui/cursor/theme`), at most
  confd's 4 KiB value, so no path is ever read by `xuid`, no file-access check
  was needed and the existing `ui/#` subscription delivers a change live. The
  built-in theme is 1.1 KB; the editor refuses an Apply that does not fit and
  says so. `.lcur` files (open/save/import/export) are not in the app yet.
- **Coordinates are design units (0..=512)**, not half cells, so the baked
  rotated built-ins fit the same format; the editor snaps to 8.
- **Colours are per layer RGB** (`fill`, `outline`), typed as hex in the
  editor (no colour picker yet).
- **Tools are buttons**, not a toggle strip; Alt+drag places freely.
