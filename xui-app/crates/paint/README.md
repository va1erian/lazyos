# xui-paint (`xpaint`)

A minimal MS-Paint-style doodler built on [xui](../xui-core), designed to be the
test vehicle for xui on a toy OS: it needs `std` and little else, has no
threads, timers, network, process or filesystem dependency, and every action is
reachable with the mouse alone.

| | |
|---|---|
| Canvas | white, 320x240 by default, up to 1024x768, scrollable/clipped |
| Tools | Pencil, Brush, Eraser, Line, Rectangle, Ellipse, Fill, Picker |
| Sizes | 1, 2, 4, 8, 16 px |
| Colours | 16 fixed swatches; left picks primary, right picks secondary |
| History | byte-bounded undo/redo (8 MiB), Clear, New |
| Status bar | cursor position, canvas size, active tool, last save/load message |
| Persistence | optional `Storage`; `MemoryStorage` by default, `FsStorage` behind `--features fs` |

## Running

```sh
cargo run -p xui-paint                 # 800x600 window on xui-canvas' WinitBackend
cargo run -p xui-paint -- --self-test  # no window, no I/O; prints PASS/FAIL
```

`--self-test` runs a scripted doodle through the model and a headless offscreen
render, prints one `PASS`/`FAIL` line per check and exits non-zero on failure.
That is the check a host without `cargo test` (a toy OS) runs.

## Layers

```text
crates/xui-paint/src/
  lib.rs              public surface; xui-core only
  model/              pure logic: Bitmap, raster, flood fill, History, tools
    bitmap.rs         RGBA8 buffer, PNG encode/decode through xui's Image
    raster.rs         Bresenham lines, rectangles, ellipses, brush stamping
    fill.rs           iterative scanline flood fill (no recursion)
    history.rs        byte-capped undo/redo snapshots
    tool.rs           Tool, Side, SIZES
    document.rs       Model: fields and accessors
    document/edit.rs  Model actions (begin/extend/end, fill, pick, undo/redo)
  view/               xui glue: PaintApp, canvas, tool strip, palette, status
    app.rs            Msg -> model glue, layout and status bar wiring
    layout.rs         the four rectangles and the Observer mirror
    canvas.rs         custom canvas node; canvas/paint.rs draws it
    toolbar.rs        custom tool strip; toolbar/paint.rs draws it
    palette.rs        custom palette
    icons.rs          hand-drawn paint-tool glyphs
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

   `main.rs` uses `xui_canvas::WinitBackend`; a toy OS supplies its own. The
   smallest native backend must implement window/event pumping, node
   create/move/show/clip, `set_painter` plus a `Canvas`, `measure_text`/
   `text_shaper`, `dpi`/`client_rect`, `invalidate`, `set_capture` and
   `supports → Painted`. `xui_canvas::OffscreenBackend` is a workable model: it
   implements the same contract over a `tiny-skia` pixmap with no window and no
   timers. See `GAPS.md` G13.

2. **Optionally, a `Storage`.** `MemoryStorage` needs nothing; `FsStorage`
   (feature `fs`) writes one configured path and refuses symlinks. A host may
   implement the trait over whatever file API it has. Save/Open cells are only
   shown when `Storage::available()` is true, and a failed save/load is reported
   in the status bar and never panics.

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

**Resize.** The last strip cell opens a prompt pre-filled with the current
`WxH` (also `W,H` / `W x H`); `Model::resize` keeps the pixels top-left, pads
white, is one undo step and rejects sides outside `1..=1024`.

`PaintApp::build_observed(ui, storage, observer)` also reports tool, size,
colours, history and status to an `Observer`, for a host that wants to mirror
them. `xui_paint::layout(client, dpi, io)` returns the four rectangles the
widgets occupy (`io` is whether the storage is available, which decides whether
Save/Open cells take space).

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

- `tests/model.rs` and the in-module unit tests: rasterization, interpolation,
  brush sizes, flood fill edges, eraser, eyedropper, undo/redo limits, the byte
  cap, PNG round-trip and hostile/out-of-range coordinates.
- `tests/view.rs` drives the app through `xui_canvas::snapshot::Stage`:
  toolbar/palette/right-button strokes, undo/redo cells, status updates and the
  drag lifecycle (mouse-up outside, a second button mid-drag, focus loss).
- `tests/snapshots.rs` renders light and dark at 96 and 192 DPI at 320x240 and
  800x600 with pixel predicates, and writes PNGs to `target/snapshots/`.
- `tests/no_io.rs` runs the app with a storage whose every method fails.
- `tests/deps.rs` asserts the library pulls in no `xui-win32`/`winit` and that
  backend selection lives only in `main.rs`.
- `tests/selftest.rs` runs the same scripted checks as `--self-test`.

See [GAPS.md](GAPS.md) for the xui limitations found while building this and the
local workarounds used.
