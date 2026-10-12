# Plan: Paint becomes an image editor (a light Paint.NET)

Paint (`os.lazy.paint`, crate `xui-app/crates/paint`, binary
`xui-app/src/bin/paint.rs`) is today an MS-Paint doodler: one white RGBA
bitmap up to 1024x1024, eight tools, 16 swatches, whole-bitmap undo snapshots
capped at 8 MiB, PNG only, no keyboard, no menu. This plan turns it into a
general-purpose image editor modelled on Paint.NET, minus its heaviest parts.

**v1** (this plan's phases P0-P6) is what is easy or medium: a menu bar and
shortcuts, the usual selection tools with cut/copy/paste, layers with opacity
and real alpha, a proper set of drawing tools including Text, simple image
operations and adjustments, and a layered file format. **v2** (section 6) is
the hard part: effects with live preview, free transform, blend modes,
tiled large-image storage, cross-app image clipboard and the rest.

## 1. Where we start

| Area | Today | Notes |
|---|---|---|
| Model | `Model { bitmap, history, tool, size, primary, secondary, preview, drag }` | `model/` is pure and host-tested; keep that split |
| Pixels | one straight RGBA8 `Bitmap`, sides clamped to `1..=1024` | eraser paints secondary/white, alpha is never edited |
| Raster | Bresenham lines, rect/ellipse outlines, round brush stamps, scanline flood fill | aliased only |
| History | a full `Bitmap` clone per action, 8 MiB cap | 4 MiB per snapshot at 1024x1024: two undo steps |
| View | custom `Control` nodes: canvas (1:1, scroll offset), tool strip, palette, status | mouse-only (GAPS G6), no zoom (G5) |
| Files | PNG through `Storage` + xui `FileDialog`; JPEG decodes (`Image::decode`) | MIME `image/png` only |
| Tests | `tests/{model,view,snapshots,files,resize,pick,no_io,selftest,deps}.rs`, session `tools/screenshot/examples/xui_paint.json` | markers `PAINT:UP/OPEN/SAVE:PASS` |

What xui gives us now that `GAPS.md` predates:

* **Menus**: `xui_core::arrange::menu_bar` with `submenu`/`item`/`MenuId`, as
  the Editor uses (`xui-app/src/bin/editor/ui.rs`).
* **Shortcuts**: `ui.on_key(|key, modifiers| ...)`, a window-level key hook
  (Writer's `shortcut`, `crates/writer/src/app.rs`). G6 is closed.
* **Colour picking**: `widget::colorpanel` (hue/field/text entry), `Slider`,
  `NumberField`, `ComboBox`, `CheckBox`, `Tabs`, `ListView`, `Dialog`.
* **Cursors**: `Cursor::{Default, Hand, Text, Size*, Busy}`; no crosshair yet.

Still open: `draw_image` always filters bilinearly (G5) and dirty rects are
ignored by the software backends. Both are handled in-app (P0).

## 2. Principles

* **Rust app, portable core.** Paint is the case AGENTS.md names for Rust.
  `model/` stays free of `Ui`, widgets and backends; every feature lands with
  host tests in `crates/paint/tests/` before a QEMU session.
* **Straight (non-premultiplied) RGBA8 layers.** Matches PNG, the colour
  picker and Paint.NET, and avoids the colour loss of repeated edits on
  premultiplied low-alpha pixels.
* **tiny-skia for coverage, our code for compositing.** Shapes, antialiased
  brush strokes, gradients and glyph outlines are rasterised by `tiny-skia`
  (pure Rust, already in the tree through `xui-canvas`) into a coverage
  `Mask` the size of the dirty rectangle; `model::blend` applies
  `coverage x selection x colour` onto the straight layer. One code path
  clips every tool to the selection.
* **One edit, one history entry, one dirty rectangle.** History stores the
  rectangle of one layer it touched (plus structural layer commands), not the
  whole image.
* **No arbitrary small caps** (the project's limits preference): image size is
  bounded by a memory budget checked at allocation with a clear message, not
  by a 1024 px side.
* Files stay under 500 lines; new modules are named below so the split is
  planned rather than discovered.

## 3. Target layout

```
+----------------------------------------------------------------------+
| File Edit View Image Layers Adjustments Help                          |  menu bar
| [tool options: size | AA | fill mode | tolerance | font ...]          |  options bar
+----+--------------------------------------------------+--------------+
|tool|                                                  | Colors       |
|grid|      canvas (zoomed, checkerboard, ants)         | prim/sec +   |
| 2x |                                                  | palette      |
| 11 |                                                  +--------------+
|    |                                                  | Layers       |
|    |                                                  | [eye] name   |
|    |                                                  | + - dup ^ v  |
|    |                                                  +--------------+
|    |                                                  | History      |
+----+--------------------------------------------------+--------------+
| 640x480 | 312,140 | sel 120x80 | 200% | Paintbrush                     |  status bar
+----------------------------------------------------------------------+
```

The right column is one vertical split; History is collapsible. At 2x
(HiDPI) everything goes through `xui_app::hidpi` design units.

## 4. Architecture changes (P0, before any feature)

### 4.1 Document and layers

```text
model/
  image/        Document { width, height, layers: Vec<Layer>, active, selection }
    layer.rs    Layer { name, pixels: Bitmap, visible, opacity: u8 }   (blend: Normal only in v1)
    flatten.rs  composite visible layers over transparent, by rectangle
  blend.rs      source-over on straight RGBA, coverage + selection mask
  selection/    Selection (see P2)
  history/      Edit enum + byte-capped stacks
  tools/        one module per tool (pencil.rs, brush.rs, shape.rs, fill.rs, ...)
  ops/          image operations and adjustments (P5)
```

`Model` keeps tool state (tool, colours, options, drag) and owns a
`Document`. A new image has one layer, "Background", opaque white or
transparent (New dialog). PNG/JPEG/BMP open as a one-layer document.

### 4.2 History as deltas

```rust
enum Edit {
    Pixels { name, layer: LayerId, rect: Rect, before: Bitmap },  // the rect only
    Selection { name, before: Selection },
    Layer(LayerCommand),           // add, delete(with pixels), move, props, merge
    Document { name, before: Box<DocumentSnapshot> }, // resize/crop/rotate: whole doc
}
```

Undo swaps `before` with the current pixels of the rectangle, so redo needs no
second copy. The cap grows to a budget (default 64 MiB, a setting later) and
keeps at least one entry. Each entry has a display name ("Paintbrush",
"Rectangle Select", "Merge Layer Down") for the History panel and the
Edit menu ("Undo Paintbrush").

### 4.3 Viewport and rendering

* `Viewport { zoom, origin }`: zoom steps 1/16x..32x, fit, actual size,
  wheel+Ctrl zooms on the pointer, Space-drag and scrollbars pan.
* The view keeps a **flattened cache** (the composite of visible layers) that
  a dirty rectangle updates, then builds a **screen image** of the visible
  viewport only: nearest-neighbour scaling done in-app (works around G5),
  over a checkerboard where alpha < 255, with the pixel grid at zoom >= 8x.
  `draw_image` then draws it 1:1. The screen image is at most the window's
  size, so the per-frame cost does not grow with the picture.
* Overlays (selection ants, shape previews, handles, text caret) are drawn
  with canvas primitives on top, in screen space, never into pixels.
* A host bench (like LazySky's `--test bench`) asserts the stroke-to-frame
  cost on a 4096x4096, 4-layer image stays under a budget (target p99 8 ms).

### 4.4 Size limits

`MAX_SIDE` goes to 16384; `Document::new` refuses a document whose
`width x height x 4 x layers` exceeds the memory budget (default 512 MiB,
guest RAM permitting) with a status-bar message. Every allocation path (new,
open, resize, add layer, paste) checks the budget first.

## 5. v1 phases

Sizes: S (a day or two), M (about a week), L (more).

### P0 - Foundation (L)

Sections 4.1-4.4: layers in the model (one visible layer to start, UI
unchanged), delta history, viewport with zoom/pan, flattened and screen
caches, checkerboard, the budget. Existing tests keep passing; snapshots gain
200% and 800% zoom cases. Exit: today's app, zoomable, with 4 MiB of history
holding hundreds of strokes instead of two.

### P1 - Menu bar, shortcuts, panels (M)

* Menu bar (`menu_bar`) with the full v1 menu below; items disable when not
  applicable (no selection, one layer, nothing to undo).
* `ui.on_key` shortcut table (Paint.NET's keys), ignored while a dialog is
  open (Writer's `dialog_open` pattern). Tool keys cycle a group on repeat
  (S: rectangle -> ellipse -> lasso -> magic wand).
* The layout of section 3: tool grid with tooltips, the options bar, Colors
  panel (primary/secondary with alpha, swap `X`, reset `D`, 32-swatch palette,
  a click on primary/secondary opens `colorpanel` with an alpha slider and
  hex entry), status bar.
* The tool strip's Save/Open/Resize cells move into the menus.

| Menu | v1 items (shortcut) |
|---|---|
| File | New... (Ctrl+N), Open... (Ctrl+O), Save (Ctrl+S), Save As... (Ctrl+Shift+S), Close/Quit (Ctrl+Q) |
| Edit | Undo (Ctrl+Z), Redo (Ctrl+Y), Cut (Ctrl+X), Copy (Ctrl+C), Copy Merged (Ctrl+Shift+C), Paste (Ctrl+V), Paste into New Layer (Ctrl+Shift+V), Paste into New Image (Ctrl+Alt+V), Erase Selection (Del), Fill Selection (Backspace), Invert Selection (Ctrl+I), Select All (Ctrl+A), Deselect All (Ctrl+D) |
| View | Zoom In (Ctrl++), Zoom Out (Ctrl+-), Zoom to Window (Ctrl+B), Actual Size (Ctrl+0), Pixel Grid |
| Image | Crop to Selection (Ctrl+Shift+X), Resize... (Ctrl+R), Canvas Size... (Ctrl+Shift+R), Flip Horizontal/Vertical, Rotate 90 CW/CCW, Rotate 180, Flatten (Ctrl+Shift+F) |
| Layers | Add New Layer (Ctrl+Shift+N), Delete Layer, Duplicate Layer (Ctrl+Shift+D), Merge Layer Down (Ctrl+M), Move Up/Down, Flip Layer H/V, Layer Properties... (F4) |
| Adjustments | Invert Colors (Ctrl+Shift+I), Black and White (Ctrl+Shift+G), Sepia (Ctrl+Shift+E), Brightness/Contrast..., Hue/Saturation... |
| Help | About Paint |

Tools: S selects, M move, Z zoom, H pan, P pencil, B brush, E eraser,
F bucket, G gradient, K picker, T text, O shapes/line, `[`/`]` brush size.

### P2 - Selections, cut/copy/paste (L)

* **`Selection`**: a bounding rectangle plus an optional 8-bit `Mask`
  (`None` = the rectangle exactly, the common case, kept cheap). v1 masks are
  hard-edged (0/255); the 8-bit type leaves room for v2 feathering.
* **Tools**: Rectangle Select, Ellipse Select, Lasso (polygon from the drag,
  filled even-odd by tiny-skia), Magic Wand (flood fill with a tolerance on
  the active layer or the image, contiguous or global; reuses `fill.rs`).
  Modes on the options bar and by modifier: Replace, Union (Ctrl),
  Exclude (right button / Alt), Intersect (Alt+left), Xor. Shift constrains
  to a square/circle. The status bar shows the selection's size.
* **Every pixel tool clips to the selection** through `blend.rs`; fill and
  gradient fill the selection when one exists.
* **Marching ants**: the mask's outline as a dashed path in screen space,
  animated by a UI timer when available, static black/white dashes otherwise.
* **Move Selected Pixels** (M): lifts the selected pixels of the active layer
  into a *floating selection* (pixels + offset, the hole left transparent),
  drags them, Arrow keys nudge 1 px (Shift: 10), commits on tool change,
  Enter or a click outside; Esc cancels. One history entry per lift-and-drop.
  **Move Selection** (M again) moves only the outline.
* **Clipboard, in-app**: Cut/Copy put a `Bitmap` + mask bounds in an app-wide
  slot; Paste creates a floating selection at the top-left of the view
  (Paint.NET's "expand canvas?" prompt when it is larger), Paste into New
  Layer / New Image as in the menu.
* **Clipboard, cross-app**: `clipboardd` offers bytes inline up to 8 KiB
  until it carries `Buffer` objects, so v1 also offers `image/png` there only
  when it fits, and always the in-app slot. A real image clipboard is v2.

### P3 - Layers and alpha (M)

* **Layers panel**: a row per layer, top first: thumbnail (cached, rebuilt on
  the layer's revision), visibility eye, name; click activates, double-click
  opens Layer Properties (name, visible, opacity 0-255 with live preview,
  Cancel restores). Buttons: add, delete, duplicate, merge down, up, down.
  Drag to reorder is a stretch goal for v1.
* **Alpha everywhere**: colours carry alpha (the Colors panel slider),
  brushes paint with it, the eraser writes transparency (Paint.NET's
  behaviour; on a document with no alpha this shows the checkerboard), the
  picker reads it, Magic Wand compares it.
* **Compositing**: Normal blend with layer opacity, straight-alpha
  source-over, in `flatten.rs`; host tests pin values against a reference
  formula (including opacity 0, alpha 0, fully opaque fast path).
* **History panel**: the list of `Edit` names, the current one highlighted;
  clicking a row undoes/redoes to it.

### P4 - Drawing tools (L)

All tools draw into a **stroke buffer** (coverage for the current stroke)
and composite it once per frame, so overlapping dabs inside one stroke do not
build up alpha, and Esc mid-drag cancels cleanly. Options bar per tool:

| Tool | v1 behaviour and options |
|---|---|
| Pencil | 1 px, aliased, exact colour (also used for pixel art); Shift-click draws a line from the last point |
| Paintbrush | round, width 1-500, antialias on/off, hardness (soft edge from a radial falloff) |
| Eraser | as Paintbrush, writes alpha 0 (right button: secondary colour) |
| Line | width, antialias, arrow heads (none/start/end), Shift snaps to 15 degrees |
| Rectangle / Rounded rectangle / Ellipse | outline, fill, or outline+fill (primary outline, secondary fill); width; corner radius; Shift constrains |
| Paint Bucket | tolerance 0-100 %, contiguous or global, sample layer or image, antialiased edge off |
| Gradient | linear, linear reflected, radial, diamond; primary -> secondary including alpha; respects the selection |
| Color Picker | sample one pixel or a 3x3/5x5 average, layer or image; option to switch back to the previous tool |
| Text | see below |
| Zoom / Pan | click zooms in (right: out), drag a rectangle zooms to it; pan by drag |

**Text tool.** Click places a caret; typing shows live, antialiased text in
the primary colour; Enter starts a new line; the options bar has font family
(ComboBox of the installed families), size, bold, italic, underline,
alignment (left/centre/right) and antialias. The text stays editable (caret,
Backspace, arrows, moving the block by its handle) until the tool changes,
Esc or a click outside commits it as pixels in one history entry, as in
Paint.NET. Rasterisation goes through a `TextRasterizer` trait in the model
(`fn layout(&self, text, &TextStyle) -> Coverage`); the crate's
`text` feature implements it with `cosmic-text` + tiny-skia from the font
bytes the host registers (the Liberation Sans/Serif/Mono families every
desktop image installs under `/system/share/fonts/liberation`, plus the Droid
fonts compiled into the xui apps), so host tests use the
repository's TTFs and see the same glyphs as the guest. Editable text
*layers* (re-open text after commit) are v2.

A crosshair and a move cursor are added to `libs/cursorgfx` and xui's
`Cursor` (closing GAPS G4); until then the canvas paints its brush ring.

### P5 - Image operations and adjustments (M)

* **Image**: Resize (by percent or pixels, keep aspect, nearest or bilinear),
  Canvas Size (anchor grid of 9, new area transparent or secondary colour),
  Crop to Selection (the selection's bounds; outside the mask becomes
  transparent), Flip/Rotate 90/180 for the image and per layer, Flatten.
* **Adjustments** (active layer, clipped to the selection): Invert Colors,
  Black and White, Sepia (instant); Brightness/Contrast and Hue/Saturation
  (a dialog with sliders and a preview of the canvas, computed on the
  screen image only while dragging, applied to the full layer on OK).
* Each is a pure function in `model/ops/` over `(&Bitmap, rect, mask)`, tested
  against hand-computed pixels.

### P6 - Files, packaging, sessions (M)

* **Native format: OpenRaster (`.ora`)**: a zip with `mimetype`,
  `stack.xml`, one PNG per layer and a merged thumbnail. It keeps layers,
  names, opacity and visibility, and GIMP, Krita and MyPaint open it. Zip
  read/write comes from `lazyarc` (`xui-app/crates/archive`), stored entries
  (PNGs are already compressed). Fuzz the reader with `lazyarc`'s seeded
  harness pattern.
* **Open**: PNG, JPEG, BMP, ORA. **Save**: ORA, PNG, BMP; saving a
  multi-layer document as PNG/BMP asks "Flatten image?" and flattens only the
  written file, not the document.
* Title shows `name.ext* - Paint`; New/Open/Quit with unsaved changes ask
  Save / Don't Save / Cancel (Writer's `files::guard`).
* Package: MIME rows for `image/png`, `image/jpeg`, `image/bmp`,
  `image/openraster` (`open`, `edit`); the Picture Viewer's Edit verb lands
  in Paint. Re-derive permissions from a `LAZYOS_LABEL_TRACE=1` run (the
  clipboard scopes `os.lazy.clipboard.read.v1`/`write.v1` are new).
* Sessions: extend `xui_paint.json` and add `xui_paint_editor.json`
  (shortcuts, rectangle select + cut + paste into a new layer, opacity,
  text, save ORA, reopen, save PNG). Markers: `PAINT:UP:PASS`,
  `PAINT:OPEN:PASS:<path>:<w>x<h>:<layers>`, `PAINT:SAVE:PASS:<path>:<format>`,
  `PAINT:LAYERS:<n>`, `PAINT:SELECTION:<x>,<y>,<w>,<h>`. Each run under
  `LAZYOS_UI_PROBE=1` so steps click widgets by name.
* Docs: this plan's status table, `crates/paint/README.md` (tools, keys,
  formats), `GAPS.md` entries closed (G4, G6) or updated (G5).

### v1 order and exit criteria

P0 -> P1 -> P2 -> P3 -> P4 -> P5 -> P6; P4's tools are independent and can be
taken in any order after P2's clip path exists, and P5 can run in parallel
with P4. v1 is done when the session above passes, the snapshot suite covers
each tool at 96 and 192 DPI in both themes, the bench holds on a 4096x4096
4-layer document, and an `.ora` written by Paint opens in GIMP or Krita with
its layers.

## 6. v2: the hard features

| Feature | Why it is hard |
|---|---|
| **Effects with live preview**: Gaussian blur, unsharp mask, sharpen, noise, median, emboss, motion/radial blur, pixelate, vignette | separable kernels and big radii need tiled, multi-threaded rendering (worker threads, cancellation on a new slider value), a dialog framework with preview, and a shared tile scheduler |
| **Curves and Levels** | a curve editor widget and histograms per channel |
| **Free transform**: scale/rotate/skew selected pixels with handles, arbitrary-angle rotate, bicubic resampling | a transform overlay with handles, resampling quality, the floating selection becoming a transformed one |
| **Blend modes** (Multiply, Screen, Overlay, Additive, Difference, ...) | per-pixel formulas are easy, but every cache, thumbnail, merge and the ORA `composite-op` must agree |
| **Tiled layer storage** | copy-on-write tiles (e.g. 256x256) so history, undo and huge images cost what changed; history spilled to `/tmp` or disk; needed past roughly 8k x 8k |
| **Cross-app image clipboard and drag and drop** | `clipboardd` carrying `Buffer` objects (MIDL change, size and lifetime policy), `image/png` offers to and from other apps, `DragStart`/`Drop` with images |
| **Feathered and antialiased selections**, select by colour globally on the image, Quick Mask | the 8-bit mask becomes soft everywhere: blend, ants (contour tracing of a soft mask), move, crop |
| **Editable shapes and text** (re-open a committed shape or text) | an object layer kind beside pixel layers, its own file representation |
| **Bezier curve tool** | handle editing UI on the canvas |
| **Clone Stamp, Recolor, smudge** | sampling offsets across layers, the stroke engine reading while writing |
| **Pen pressure** | xui has no pen/pressure events (GAPS G11) |
| **More formats**: JPEG save with quality, GIF, WebP, TIFF, ICO; PSD/PDN import | encoders and parsers, each fuzzed |
| **Scripting**: Rhai effects and macros over the `model/ops` API | an API surface to keep stable, sandboxing of the script |
| **Printing** through `printd` | small, reuses Writer's path; listed here only to keep v1 tight |
| **Screen capture** ("Acquire from screen" through `xuid`) | a new display permission and its policy |

## 7. Risks

* **Repaint cost.** Software backends repaint the whole canvas node per event
  (G5). The screen-image cache bounds the work to the window; if the bench
  fails, the fix is a real dirty rect in `xui-canvas`, not a smaller image cap.
* **Memory in a 1 GiB guest.** Four 4096x4096 layers are 256 MiB plus
  history; the budget check and a clear message matter more than the max side.
* **Text fidelity.** Using the same `cosmic-text` stack as xui keeps guest and
  host renders identical; a separate glyph rasteriser would drift.
* **Scope creep from Paint.NET.** Anything not in a v1 phase above goes to the
  v2 table first.
