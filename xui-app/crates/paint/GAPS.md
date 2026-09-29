# Gaps found while building `xpaint`

`xpaint` is a paint app, so it probes parts of xui that the widget catalogue does
not cover. Each entry below was checked against the source; "not a gap" is a
real result too. `// xui gap: G<n>` markers in the tree point here.

## G1 — Custom-painted widget with mouse events

**Not a gap.** `Control` is public (`crates/xui-core/src/widget/control.rs:30,51,56`)
and, with `NodeSpec::new(NodeKind::Custom, bounds)`
(`crates/xui-core/src/backend/node.rs:38,132`), gives exactly a painted node that
maps `Event`s to the app's `Msg` via `Control::on_events`. What is missing is only
a bespoke `CustomWidget` builder with `on_mouse_down/move/up` methods; the
primitive is there. **Workaround: none.** `view/canvas.rs`, `view/toolbar.rs`
and `view/palette.rs` each hold a `Control<Msg>` directly.

**Proposal:** a small `widget::Canvas` wrapper in `xui-core` that bundles a
`Control`, a painter closure and `on_mouse_*` builders, following the closure-to-
`Msg` rule — pure ergonomics, no new backend capability.

## G2 — Mouse capture during a drag

**Not a gap.** `Ui::set_capture`/`release_capture`
(`crates/xui-core/src/app/ui/node_ops.rs:99,104`) map to the `Backend` contract
(`crates/xui-core/src/backend/mod.rs:254,260`); the offscreen backend honours it
(`crates/xui-canvas/src/offscreen/backend.rs:167,424`) and re-targets moves and
releases to the captured node. Tests
`a_mouse_up_outside_the_widget_ends_the_drag` and
`a_second_button_mid_drag_does_not_stick` cover it. **Workaround (one line):**
`release_capture` synchronously delivers `Event::CaptureChanged`, which would
read as a new interruption; `view/canvas.rs` sets a `releasing` flag around the
release so its own `CaptureChanged` is ignored. Marked `// xui gap: G2`.

**Proposal:** `Backend::release_capture` could suppress its own synthetic
`CaptureChanged`, or the event could carry a "solicited" flag, so no caller needs
the guard.

## G3 — Which button and modifiers arrive

**Not a gap.** `Event::MouseDown/MouseUp/MouseMove` carry `button` and
`modifiers` (`crates/xui-core/src/backend/event.rs:19,30,44`), which is how the
right button picks the secondary colour. (The *widget layer's* `ColorPicker`
discards them — see G9.)

## G4 — Crosshair pointer cursor

**Partial gap.** `Ui::set_cursor` and `Cursor`
(`crates/xui-core/src/app/ui/node_ops.rs:87`,
`crates/xui-core/src/backend/cursor.rs`) exist, and the winit backend stores a
per-node cursor (`crates/xui-canvas/src/backend/contract.rs:246`). But there is
no `Cursor::Crosshair`, which a paint canvas asks for, and the offscreen backend
has no `set_cursor` override at all, so tests cannot assert one. **Workaround:**
`view/canvas.rs` paints its own brush-sized ring at the pointer instead of
relying on the OS cursor. Marked `// xui gap: G4`.

**Proposal:** add `Cursor::Crosshair` to `xui-core` and map it in the canvas
backend's per-node cursor lookup; a painted fallback is fine on backends with no
cursors.

## G5 — Scaled images and dirty-rect repaint

**Gap.** `Canvas::draw_image` always scales with `FilterQuality::Bilinear`
(`crates/xui-core/src/backend/canvas.rs:318`,
`crates/xui-canvas/src/canvas.rs:324`), with no nearest-neighbour choice, so a
future zoom would blur hard-edged paint. `Ui::invalidate_rect` exists
(`crates/xui-core/src/app/ui/node_ops.rs:168`) but both software backends no-op
it (`crates/xui-canvas/src/backend/contract.rs:333`,
`crates/xui-canvas/src/offscreen/backend.rs:222`), and a `Painter` always
receives the whole node — one pencil segment repaints the whole canvas node.
**Workaround:** draw the bitmap 1:1 at the scroll offset and cache the
`Image`, rebuilding it only when the model's `revision` changes, so the repaint
does no per-event allocation. Marked `// xui gap: G5`.

**Proposal:** a `draw_image_filtered(&Image, Rect, ImageFilter)` (or a
`Filter` on the canvas state) in `xui-core`, and a dirty rect on a painted node
that `invalidate_rect` records and a backend intersects at paint time.

## G6 — Keyboard shortcuts and accelerators

**Gap.** The vocabulary has `Event::KeyDown` and `Event::Accelerator(u16)`
(`crates/xui-core/src/backend/event.rs:80,167`), but there is no portable
accelerator registration (no `Ui::on_accelerator`; a grep of `app/` finds none)
and no window-level key mapper. A widget can only handle `KeyDown` while
focused. **Workaround:** the app is deliberately mouse-only; every action is a
clickable cell. Marked `// xui gap: G6`.

**Proposal:** `Ui::on_accelerator(Key, Modifiers, Fn() -> Option<M>)` in
`xui-core`, resolved by the runtime before routing, and wired by each backend's
key path (the Win32 `ACCEL` table, `winit`'s key events).

## G7 — Toolbar toggle state and paint-tool icons

**Gap.** `Toolbar` has no persistent pressed/selected state: its `activated`
index is exposed only as a `Property` (`crates/xui-core/src/widget/toolbar/mod.rs:76,360`)
and hover is the only visual. The vendored Lucide set
(`crates/xui-core/assets/lucide/`) has `pencil`, `square`, `circle-dot`,
`undo-2`, `redo-2`, `trash-2`, `file-plus`, `save`, `folder-open`,
`mouse-pointer-2` — but no brush, eraser, fill/bucket, dropper, line or ellipse.
**Workaround:** `view/toolbar.rs` is a `NodeKind::Custom` strip painting its own
active/hover/pressed fills, and `view/icons.rs` hand-draws the six missing
glyphs from canvas primitives. Marked `// xui gap: G7`.

**Proposal:** `Toolbar::checkable(index)` + `Toolbar::set_active(index)` for a
radio-style group in `xui-core`, and vendor Lucide's paint icons
(`brush`, `eraser`, `paint-bucket`, `pipette`, `pen-line`, `circle`) into the
generator.

## G8 — Portable file open/save dialog

**Gap.** `Dialog` and `TaskDialog` are in-window modals
(`crates/xui-core/src/widget/dialog.rs`), but there is no portable file picker
or any `Storage`-aware dialog. **Workaround:** `storage.rs` Save/Open cells with
a host-configured path; failures surface in the status bar. A portable picker
would need host policy, so it does not belong in `xui-core`.

**Proposal:** define a `FileDialog` capability in a host crate (like
`xui-paint`'s `Storage`) implemented per platform, and leave `xui-core` a
`Dialog`-based fallback for a path text field.

## G9 — Colour picker beyond a fixed swatch grid

**Gap.** `ColorPicker` takes a fixed `&[Color]` and reports
`on_select(Color)`; it has no RGB entry, and `MouseDown` handling ignores the
button and modifiers (`crates/xui-core/src/widget/colorpicker.rs:125,176`).
**Workaround:** `view/palette.rs` is a custom node over 16 fixed colours that
maps left/right to the primary/secondary colour and paints the active pair.
Marked `// xui gap: G9`.

**Proposal:** `ColorPicker::on_select_with(|color, button, modifiers| ...)` in
`xui-core`, plus an optional "custom…" swatch that opens a small RGB popup
reusing the existing `Popup` elevation helper.

## G10 — A `Custom` node inside a `ScrollView`

**Not a gap.** `ScrollView::add(WidgetId, Dip)` registers any node id, including
a `Custom` one, as a fixed-height row and clips/scrolls it
(`crates/xui-core/src/widget/scrollview.rs:142`). The row height is fixed rather
than content-sized, and the view clips descendants rather than the painter's
own drawing — for a bitmap canvas the app instead scrolls and clips itself
(`view/canvas.rs`), which is why this crate does not depend on it. Both are
workable.

## G11 — Touch, pen and pressure

**Gap.** The `Event` vocabulary (`crates/xui-core/src/backend/event.rs`) has no
touch, pen or pressure variant. **Workaround:** pointer only. Marked
`// xui gap: G11`.

**Proposal:** `Event::Pointer { source: PointerSource, pressure: u16, ... }` in
`xui-core`, decoded from `WM_POINTER*` and `winit`'s tablet events, folded into
the existing mouse handling where pressure is absent.

## G12 — Reading the app's model in a test

**Partial gap.** `Stage::inject(Event)` can send a full drag with any button and
key events reach the focused node (`crates/xui-canvas/src/snapshot/stage.rs:50`),
so interaction is testable. But the app is owned by the runtime inside
`render_with` and there is no public `Ui`/`Runtime` constructor to hold it, so a
test cannot read the model itself. **Workaround:** `view::Observer`, a
`Rc<RefCell<_>>` mirror passed to `PaintApp::build_observed` that the app updates
after every message. Marked `// xui gap: G12`.

**Proposal:** a `xui-canvas::snapshot::Harness` that runs the build closure and
returns a typed handle to the app alongside the `Stage`, so tests assert on the
app directly instead of a probe.

## G13 — What a native lazyOS backend must supply

**Gap.** `xui-canvas` compiles `winit`, `softbuffer`, `glutin` and `arboard`
unconditionally (`crates/xui-canvas/Cargo.toml`), so `OffscreenBackend` — which
needs no window system — still cannot be built without one.

The smallest native lazyOS `Backend` must implement: window creation, an event
loop pump with input decoding, `create`/`destroy`/`apply_moves`/`set_visible`/
`set_clip`, `set_painter` plus a `Canvas` (tiny-skia is a pure-Rust CPU
rasterizer and suffices for a framebuffer), `measure_text`/`text_shaper`
(cosmic-text, needing fonts), `dpi`/`client_rect`, `invalidate`,
`set_capture`, `supports → ImplKind::Painted`, and no timers. `OffscreenBackend`
is a workable starting point: copy its `offscreen/` module plus `Surface`
(tiny-skia pixmap) into a framebuffer crate and blit `Surface::to_image` to the
screen. Marked `// xui gap: G13`.

**Proposal:** split `xui-canvas` into an always-available `surface`/`offscreen`
core and a feature-gated `winit` backend, so a toy OS can depend on the core
alone.

## G14 — `std` APIs a shim must get right

- **Threads:** `crates/xui-core/src/app/proxy.rs:168` (the worker `Proxy`).
  `xpaint` never uses `Proxy`, so no thread is spawned.
- **Time:** `Instant` for timers and double-click in the winit path
  (`crates/xui-canvas/src/backend/contract.rs:388`,
  `crates/xui-canvas/src/backend/app/input.rs:199`). `OffscreenBackend` has no
  timers and no `Instant` use.
- **Env / process / fs:** the snapshot gallery
  (`crates/xui-canvas/src/snapshot/gallery.rs:27,104`) and
  `Image::save_png` (`crates/xui-core/src/image.rs:148`). `xpaint` uses
  `Image::encode_png`/`decode` only, and its `Storage` defaults to memory, so
  the running app touches no env, process, fs or time API.
- **No `SystemTime`** anywhere in either crate.
- **Fonts:** `crates/xui-canvas/src/text.rs:40` (`FontSystem::new()`) reads the
  system font database.

**Proposal:** keep these behind the winit/snapshot layers and document that a
minimal framebuffer backend plus `xui-core` needs none of them.

## G15 — Text with no installed fonts

**Gap.** xui bundles no font. `xui-canvas` shapes with cosmic-text's system font
database (`crates/xui-canvas/src/text.rs:40`) and `xui-core`'s
`UnsupportedShaper` returns empty layouts (`crates/xui-core/src/backend/text.rs`).
With no fonts, labels render nothing. **Workaround:** every action is a
painted icon or swatch; the tool strip, palette and canvas need no text, and the
status bar simply shows blank parts. Drawing, tool/colour/size changes, undo,
clear and new all remain mouse-reachable. Marked `// xui gap: G15`.

**Proposal:** an optional embedded fallback font (or a built-in box-glyph
shaper) in `xui-canvas`, selected when `FontSystem` finds no faces, so text
degrades to legible boxes instead of vanishing.
