# xui-paint (`xpaint`)

A minimal MS-Paint-style doodler built on [xui](../xui-core), designed to be the
test vehicle for xui on a toy OS: it needs `std` and little else, has no
threads, network, process or filesystem dependency. It is growing into an image
editor modelled on Paint.NET ([`docs/paint-plan.md`](../../../docs/paint-plan.md));
this is the P2 state: menu bar, keyboard shortcuts, the panel layout, selections
and the clipboard.

| | |
|---|---|
| Canvas | white, 320x240 by default, sides up to 16384 within a 512 MiB budget (layers + cache + undo), zoom 1/16x-32x (Ctrl+wheel), pixel grid from 800% |
| Tools | Pencil, Brush, Eraser, Line, Rectangle, Ellipse, Fill, Picker, Rectangle / Ellipse / Lasso Select, Magic Wand, Move Selected Pixels, Move Selection (tool grid, left) |
| Selections | a bounding rectangle plus an optional 8-bit mask (hard-edged in v1); every pixel tool, the bucket, Fill and Erase Selection clip to it; marching ants drawn over the picture |
| Sizes | 25 widths from 1 to 500 px (`model::SIZES`: 1-6, 8, 10, 12, 16, 20, 24, 32, 40, 48, 64, 80, 100, 128, 160, 200, 256, 320, 400, 500; options bar, for the tools that use one; `[` and `]` step through them) |
| Colours | 32 swatches; left picks primary, right picks secondary; click a swatch of the pair to edit it |
| Menus | File, Edit, View, Image, Layers, Adjustments, Help; rows of unbuilt features are disabled |
| History | delta undo/redo (rectangle of the touched layer, 64 MiB cap), Clear Image, New |
| Status bar | cursor position, canvas size, active tool, last message (or the hovered tool), zoom |
| Persistence | optional `Storage`; `MemoryStorage` by default, `FsStorage` behind `--features fs` |
| Files | Open PNG, JPEG, BMP, OpenRaster; Save OpenRaster, PNG, BMP by extension (JPEG is open only); title `name.ext* - Paint`; New, Open and Quit ask Save / Dont Save / Cancel while there are unsaved changes |

```text
+--------------------------------------------------------+
| File Edit View Image Layers Adjustments Help           |  menu bar
| Brush    Size: [8 px]                                  |  tool options bar
+----+----------------------------------+----------------+
|tool|                                  | primary/second |
|grid|   canvas (or the colour editor)  | swap, reset    |
| x2 |                                  | 32 swatches    |
+----+----------------------------------+----------------+
| 100, 40 | 320 x 240 | Brush | message                  |  status bar
+--------------------------------------------------------+
```

### Keys

Paint.NET's keys, from one table (`view/shortcuts.rs`, `BINDINGS`; the menus
print the same chords). Nothing is taken while a dialog, the colour editor or a
menu is open, and plain keys (letters, Delete, Backspace, the arrows) are left to
a number or text field in the options bar while it has the keyboard: only Ctrl
chords and the function keys are claimed there.

| Keys | Action |
|---|---|
| Ctrl+N / O / S, Ctrl+Shift+S, Ctrl+Q | New, Open, Save, Save As, Quit |
| Ctrl+Z, Ctrl+Y (or Ctrl+Shift+Z) | Undo, Redo |
| Ctrl+R, Ctrl+Shift+R | Image > Resize (percent or pixels, aspect, nearest or bilinear), Canvas Size (width, height, 9-cell anchor, new area transparent or secondary) |
| P, B, E, F, K | Pencil, Brush, Eraser, Fill (bucket), Color Picker |
| O (repeat to cycle) | Line, Rectangle, Ellipse |
| `[`  `]` | Brush size down, up |
| X, D | Swap, reset the primary and secondary colours |
| S (repeat to cycle) | Rectangle Select, Ellipse Select, Lasso, Magic Wand |
| M (repeat to cycle) | Move Selected Pixels, Move Selection |
| Z, H, G, T | Zoom, Pan, Gradient, Text |
| Ctrl+A / D / I | Select All, Deselect, Invert Selection |
| Del, Backspace | Erase Selection, Fill Selection (primary colour) |
| Ctrl+X / C / Shift+C | Cut, Copy, Copy Merged |
| Ctrl+V / Shift+V / Alt+V | Paste (floating), Paste into New Layer, Paste into New Image |
| Ctrl+Shift+X | Crop to Selection |
| Arrows (Shift: 10 px), Enter, Esc | with a move tool active or pixels floating: nudge, land, cancel |
| Ctrl++ / Ctrl+- / Ctrl+B / Ctrl+0 | Zoom in, out, to window, actual size (View menu; Pixel Grid toggles the grid drawn from 800%) |
| Ctrl+Shift+F, Ctrl+Shift+N/D, Ctrl+M, F4 | Flatten, New Layer, Duplicate Layer, Merge Layer Down, Layer Properties |
| Ctrl+Shift+I / G / E | Invert Colors, Black and White, Sepia (instant, clipped to the selection) |
| Enter, Esc | OK, Cancel in the Resize, Canvas Size, Brightness/Contrast, Hue/Saturation, colour editor and Layer Properties frames (Enter does nothing extra when a button has the keyboard: the button answers) |

A key for a feature that does not exist yet (`view::Command`, whose
`available()` is false) only says so in the status bar, and its menu row is
disabled. A later phase implements it by flipping `available` and adding an arm
in `PaintApp::update`; the menu and the key table need no change.

### Selections and the clipboard

Selection tools: drag a rectangle or ellipse (Shift: square or circle), draw a
lasso (a polygon filled by the even-odd rule), or click with the Magic Wand
(options bar: tolerance in percent, contiguous or global, on the active layer;
a difference equal to the tolerance is included). A plain click without a drag
deselects. Modes: the options bar's Replace / Union / Exclude / Intersect / Xor,
or by modifier: Ctrl = Union, right button = Exclude, Alt + left = Intersect,
Ctrl + right = Xor. The status bar shows the selection's size.

Move Selected Pixels lifts the selected pixels of the active layer (the whole
layer when nothing is selected) into a floating selection and leaves a
transparent hole; drag or use the arrows, then Enter, a click outside, or any
other tool lands it as one history entry; Esc puts everything back. Paste makes
the same floating selection at the top-left of the view; a picture larger than
the canvas asks whether to expand the canvas. The clipboard is one in-app slot
counted against the memory budget (a copy that does not fit is refused). A host
may wire an `image/png` system clipboard through `view::SystemClipboard`
(`PaintApp::set_system_clipboard`); LazyOS offers copies that fit
`clipboardd`'s 8 KiB inline cap and adopts a picture another app copied.

The marching ants crawl on a timer (`Control::set_timer`) only while a selection
exists; a backend with no timers leaves them still.

## Running

```sh
cargo run -p xui-paint                 # 800x600 window on xui-canvas' WinitBackend
cargo run -p xui-paint -- --self-test  # no window, no I/O; prints PASS/FAIL
```

`--self-test` runs a scripted doodle through the model and a headless offscreen
render, prints one `PASS`/`FAIL` line per check and exits non-zero on failure.
That is the check a host without `cargo test` (a toy OS) runs.

## Sessions on LazyOS

Both run on a desktop image built with `LAZYOS_DESKTOP=1 LAZYOS_XUI_AUTOSTART=paint
LAZYOS_UI_PROBE=1` (add `LAZYOS_LABEL_TRACE=1` to derive permissions) and click
controls by name:

```sh
python tools/screenshot/qemu_session.py --image target/lazyos.img --out shots/paint     --script tools/screenshot/examples/xui_paint.json            # draw, undo, colour editor, save PNG
python tools/screenshot/qemu_session.py --image target/lazyos.img --out shots/paint_editor     --script tools/screenshot/examples/xui_paint_editor.json     # layers, text, gradient, ORA, PNG
```

Markers: `PAINT:FONTS:<n>` (Liberation faces read for the Text tool),
`PAINT:UP:PASS`, `PAINT:OPEN:PASS:<path>:<w>x<h>:<layers>`,
`PAINT:SAVE:PASS:<path>:<ora|png|bmp>`, `PAINT:SAVE:FAIL:<path>`. With the
probe the app prints `UI:WIDGET` lines (`tool:<Label>`, `palette:<n>`,
`swatch:primary`, `canvas`, `menubar`, `layer:<n>` counted from the bottom,
`props_*`, `color_ok`, `resize_*`, `adjust_*`), only when a rectangle changes.
Window names are the current title (`untitled* - Paint`).

## Layers

```text
crates/xui-paint/src/
  lib.rs              public surface; xui-core only
  model/              pure logic: Bitmap, similar-colour regions, History, tools
    bitmap.rs         RGBA8 buffer, PNG encode/decode through xui's Image
    fill.rs           similar-colour spans for the Magic Wand (no recursion)
    history.rs        byte-capped undo/redo snapshots
    tool.rs           Tool (19 tools), Side, SIZES (the `[`/`]` ladder)
    image/            Document and Layer, flatten by rectangle, memory budget
    blend.rs          straight-alpha source-over
    history/          Edit deltas and the byte-capped History
    document.rs       Model: fields and accessors
    document/edit.rs  Model actions: begin/extend/end/cancel dispatch by tool
    document/strokes.rs     Pencil, Paintbrush, Eraser: one StrokeBuffer per drag
    document/shape_drag.rs  Line, Rectangle, Rounded Rectangle, Ellipse (live preview)
    document/fills.rs       Gradient (live), Paint Bucket, Color Picker
    document/live.rs        a preview painted into the layer and taken back per frame
    document/text.rs        the Text tool: block, caret, live pixels, commit/cancel
    document/tool_options.rs  the option structs the options bar binds to
    document/image_ops.rs  resize, canvas size, crop, flip/rotate, flatten, merge down
    document/adjust.rs     Adjustments on the active layer; document/io.rs open/save bytes
    ops/              pure image operations: resample, flips/turns, adjustments (P5)
    tools/            P4 building blocks: StrokeBuffer, brush, AA shapes (tiny-skia), gradient,
                      bucket mask, picker, option structs; the document/ tools drive them
    text/             TextRasterizer trait, TextEdit; `text` feature adds the cosmic-text rasterizer
                      (off by default; the default build links tiny-skia only)
    format/           OpenRaster, BMP, magic-byte dispatch, a stored/inflate zip (P6)
  view/               xui glue: PaintApp, menu, shortcuts, panels, canvas
    app.rs            Msg -> model glue and status bar wiring
    app/              dialogs.rs (pickers, resize, About), files.rs, colors.rs
                      (colour editor flow), probe.rs (UI probe hook), canvas_msgs.rs
                      (canvas input, the Zoom tool), setup.rs (`Setup`: what a host
                      chooses, incl. the Text tool's rasterizer), status.rs
    layout.rs         the window layout (menu, options, grid, canvas, Colors,
                      status) and the Observer mirror
    menu.rs           the seven menus as one table; ids, enabling
    shortcuts.rs      the key table and the pure `shortcut` function
    command.rs        commands of features a later phase builds (disabled)
    options.rs        the tool options bar: one control per option, shown per tool
                      (options/shown.rs), changed by `OptMsg` (options/msg.rs)
    canvas.rs         custom canvas node; canvas/paint.rs draws it
    toolbar.rs        custom tool grid (two columns); toolbar/paint.rs draws it
    palette.rs        the Colors panel: swatch pair, swap/reset, 32 colours
    coloredit.rs      the colour editor (xui ColorPanel + an alpha slider) over the canvas
    layers.rs         the Layers panel: custom rows (eye, thumbnail, name); layers/thumb.rs
    sidebar.rs        right column: Layers + button strip + collapsible History list
    layerprops.rs     Layer Properties frame (name, visible, opacity; live preview)
    keyfocus.rs       which of the window's own controls has the keyboard (plain keys)
    app/saved.rs      unsaved changes: the history position when the file was written
    icons.rs          hand-drawn paint-tool glyphs
  viewport.rs         zoom and scroll arithmetic (pure)
  render.rs           flattened cache and the nearest-neighbour screen image (pure)
  storage.rs          Storage trait, MemoryStorage, FailingStorage, FsStorage
  selftest.rs         the scripted PASS/FAIL checks
  main.rs             backend selection and the binary
```

The `model` layer has no `Ui`, widget or backend type (it uses only xui's
portable `Image` for PNG). The `view` layer is thin glue mapping input events to
`view::Msg` and from there into model calls.

## Embedding the library in another host

`xui-paint`'s library depends on `xui-core` only, so a host provides the two
things an app normally gets from xui:

1. **A `Backend`.** Give `PaintApp::build` (or `build_observed`) a `&mut Ui` and
   a `std::rc::Rc<dyn Storage>`, then run it with `xui_core::app::run_app`:

   ```rust
   use std::rc::Rc;
   use xui_core::app::run_app;
   use xui_core::backend::{Backend, PlatformSpec};
   use xui_core::Dip;
   use xui_paint::storage::MemoryStorage;
   use xui_paint::view::PaintApp;

   # fn demo(backend: Rc<dyn Backend>) -> xui_core::backend::Result<()> {
   run_app(backend, PlatformSpec::new("xpaint").size(Dip(800.0), Dip(600.0)), |ui| {
       PaintApp::build(ui, Rc::new(MemoryStorage::new())).expect("build")
   })
   # }
   ```

   The LazyOS binary (`xui-app/src/bin/paint.rs`) supplies the backend; a toy OS supplies its own. The
   smallest native backend must implement window/event pumping, node
   create/move/show/clip, `set_painter` plus a `Canvas`, `measure_text`/
   `text_shaper`, `dpi`/`client_rect`, `invalidate`, `set_capture` and
   `supports → Painted`. `xui_canvas::OffscreenBackend` is a workable model: it
   implements the same contract over a `tiny-skia` pixmap with no window and no
   timers. See `GAPS.md` G13.

2. **Optionally, a `Storage`.** `MemoryStorage` needs nothing; `FsStorage`
   (feature `fs`) writes one configured path and refuses symlinks. A host may
   implement the trait over whatever file API it has. The File menu's Open, Save
   and Save As rows are enabled only when `Storage::available()` is true (or file
   dialogs are wired), and a failed save/load is reported in the status bar and
   never panics.

**File dialogs.** `PaintApp::build_with_files(ui, storage, fs)` (or
`build_with_files_observed`) wires the xui `FileDialog` for Open and Save As over
a `xui_core::widget::FileSystem`, provided the storage `supports_paths()` (the
`Storage::save_to`/`load_from` seam; the defaults are "unsupported"). Save writes
to the current file, or asks with Save As when there is none (`.png` is appended
when missing); Open decodes before it swaps, so a bad file or a failed write
leaves the document intact and reports in the status bar. `Msg::OpenStartup`
loads the storage's own `default_path()` with no dialog. `set_start_dir` picks
the pickers' starting directory. Plain `build`/`build_observed` keep the
path-less behaviour.

**Resize.** Image > Resize... (Ctrl+R) scales the picture; Image > Canvas
Size... (Ctrl+Shift+R) opens a dialog with the width and height, a 9-cell anchor
and a fill for the new area (transparent or the secondary colour). Both are one
undo step and take sides from 1 to `MAX_SIDE` (16384) within the memory budget.

**Colours.** A left click on the primary or secondary swatch opens the colour
editor over the canvas: xui's `ColorPanel` (HSV field, hue slider, hex/RGB/CMYK
boxes) with OK and Cancel. The colour changes live; Cancel restores it.

**UI probe.** `PaintApp::set_probe(hook)` plus `Msg::Probe` report the named
control rectangles (`canvas`, `menubar`, `size`, `tool:<Label>`,
`swatch:primary|secondary`, `swap_colors`, `reset_colors`, `palette:<n>`,
`color_ok`, `color_cancel`) through the hook; the LazyOS binary prints them as
`UI:WIDGET` lines in a `LAZYOS_UI_PROBE=1` image so
`tools/screenshot/examples/xui_paint.json` clicks by name.

`PaintApp::build_observed(ui, storage, observer)` also reports tool, size,
colours, history, the colour being edited and status to an `Observer`, for a
host that wants to mirror them.

`PaintApp::build_with(ui, Setup::new(storage).with_files(fs).with_observer(o)
.with_text(rasterizer))` takes every host choice at once. The **Text tool**
needs a `TextRasterizer` (`model::text`); with the `text` feature
`CosmicRasterizer` implements it from the font bytes the host registers
(`register_font`; the LazyOS binary registers the Liberation faces from
`fhs::share::FONTS_LIBERATION` and the compiled-in Droid faces). The family
list of the options bar is `TextRasterizer::families()`, read once at build.
Without a rasterizer the Text tool says so in the status bar and does nothing.

## Tools (P4)

Every drawing tool paints through a `StrokeBuffer` (or, for shapes, gradients
and text, a live preview in the layer that is taken back and repainted per
frame, `model/document/live.rs`), clips to the selection, records one history
entry per action and is cancelled by Esc mid-drag.

| Tool (key) | Options | Notes |
|---|---|---|
| Pencil (P) | none | exact colour, alpha included; Shift-click draws from the last point |
| Paintbrush (B), Eraser (E) | width 1-500, hardness, antialias | the eraser's other button paints the secondary |
| Line, Rectangle, Rounded Rectangle, Ellipse (O cycles) | width, antialias, arrows / outline-fill-both / radius | Shift: 15 degree line, square, circle |
| Paint Bucket (F) | tolerance, contiguous or global, layer or image | |
| Gradient (G) | linear, reflected, radial, diamond | primary to secondary, right button reversed; the selection or the layer |
| Color Picker (K) | 1 / 3x3 / 5x5, layer or image, switch back | |
| Text (T) | font, size, B I U, alignment, antialias | click, type, Enter = new line, arrows, Home/End, handle drags the block; Ctrl+Enter, a tool change or a click outside commits, Esc cancels; letter keys are text while editing |
| Zoom (Z), Pan (H) | none | click / right click / drag a rectangle; drag |

`[` and `]` step the width along `model::SIZES`.

## lazyOS notes

- Build without the backend to get the pure `xui-core` library:

  ```sh
  cargo check -p xui-paint --no-default-features
  cargo check -p xui-paint --no-default-features --features fs
  ```

- The closest offline proxy for the target is the musl Linux check:

  ```sh
  cargo check -p xui-paint --target x86_64-unknown-linux-musl
  ```

  This is documented as a proxy only; it is not required to run here.
- The running app uses no thread, timer, env var, process or file API; the
  `Storage` default is in-memory. `--self-test`'s offscreen render needs the
  `canvas` feature, so on a host with no window system build
  `--no-default-features` and the model self-test still runs (see GAPS G13).

## Tests

All cross-platform, no real window:

- `tests/model.rs` and the in-module unit tests: interpolation,
  brush sizes, eraser, eyedropper, undo/redo limits, the byte
  cap, PNG round-trip and hostile/out-of-range coordinates.
- `view/shortcuts/tests.rs` and `view/menu/tests.rs`: the key table (coverage,
  no collisions, nothing while a dialog is open, tool cycling) and the menu table
  (ids, the plan's items, mnemonics, enabled rows).
- `tests/view.rs` drives the app through `xui_canvas::snapshot::Stage`: tool
  grid and palette clicks, keys, the menu bar, the colour editor, strokes,
  status updates and the drag lifecycle (mouse-up outside, a second button
  mid-drag, focus loss).
- `tests/tools_model.rs`, `tests/tools_fills.rs` and `tests/tools_text.rs` drive
  every P4 tool through the model (one undo step, Esc, selection clip, hostile
  coordinates, a fake box rasterizer for the text editing).
- `tests/tools_view.rs` does it through the window (Shift, Esc, Zoom, Pan, the
  options bar's controls, and with `--features text` real glyphs typed with key
  events); `tests/tools_snapshots.rs` makes the soft brush, shape, gradient and
  text pictures at both themes and DPIs (`target/snapshots/paint-p4-*`).
- `tests/probe.rs` clicks the centre of every probed rectangle, as `click_at` does.
- `tests/snapshots.rs` renders light and dark at 96 and 192 DPI at 320x240 and
  800x600 (plus the Brush options bar and the colour editor) with pixel
  predicates, and writes PNGs to `target/snapshots/`.
- `tests/no_io.rs` runs the app with a storage whose every method fails.
- `tests/deps.rs` asserts the library pulls in no `xui-win32`/`winit` and that
  backend selection lives only in `main.rs`.
- `tests/selftest.rs` runs the same scripted checks as `--self-test`.

See [GAPS.md](GAPS.md) for the xui limitations found while building this and the
local workarounds used.

## Image operations, adjustments and file formats

`Model` methods, each one named undo step: `resize_image`, `canvas_size`,
`crop(Rect)`, `flip_image`/`rotate_image`, `flip_layer`/`rotate_layer`,
`flatten_image`, `merge_layer_down`, `adjust(Adjustment)` /
`adjust_in(Adjustment, Rect)`, and `open_bytes` / `save_bytes(Format)` /
`needs_flatten(Format)`. The pure functions are in `model::ops`; the formats
in `model::format` (open by magic bytes, save by `Format::from_path`).

| Format | Open | Save | Layers |
|---|---|---|---|
| OpenRaster `.ora` | yes | yes | yes (name, opacity, visibility, active) |
| PNG | yes | yes | flattened copy |
| BMP (1/4/8/24/32 bit) | yes | yes (24 bit, or 32 bit with alpha) | flattened copy |
| JPEG | yes | no | flattened |

### The dialogs and the file flow

xui `Dialog` holds only text and one entry (GAPS G17), so Resize, Canvas Size,
Brightness/Contrast and Hue/Saturation are titled frames in the window
(`view/dialogs/`). Resize and Canvas Size take the canvas place; the two
adjustment strips sit under it so the picture stays in view. Each field is
validated when read; a refusal (a bad number, or the memory budget) goes to the
status bar and leaves the dialog open. Brightness/Contrast and Hue/Saturation
preview on the **screen image only** (`PaintCanvas::set_adjust_preview`, the
`render::render_tweaked` hook): a drag costs the window, not the layer, and the
document and its history are untouched until OK, which applies the adjustment to
the active layer, through the selection, as one history entry. The preview
adjusts the composite, so with layers showing through the active one it
approximates what OK produces.

Save picks the format from the extension. A name with none gets `.ora` when
the picture has layers and `.png` otherwise. Saving layers as PNG or BMP asks
"Flatten image?" and flattens only the file written. JPEG says it cannot be
saved yet. The unsaved state is the model revision against the one at the last
open or save; a selection is a history step, so it counts as a change, like in
Paint.NET.

Evidence the LazyOS binary prints (through `PaintApp::set_reporter`):
`PAINT:OPEN:PASS:<path>:<w>x<h>:<layers>` and `PAINT:SAVE:PASS:<path>:<format>`
with the format `ora`, `png` or `bmp`.

Tests: `tests/image_dialogs.rs`, `tests/adjust_dialogs.rs`,
`tests/file_formats.rs` (every dialog and flow through real events over a
path-aware in-memory store, `tests/common/files.rs`) and
`tests/dialog_snapshots.rs` (Resize, Canvas Size and Hue/Saturation, light and
dark, 96 and 192 DPI, in `target/snapshots/`).
