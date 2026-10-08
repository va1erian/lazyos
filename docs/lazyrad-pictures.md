# Picture Viewer (`os.lazy.pictures`)

A picture viewer in the manner of Windows XP's Picture and Fax Viewer,
written in LazyRAD: one form (`main_form.lfm`), its Rhai script and a small
path module, in [`lazyrad-os/samples/pictures`](../lazyrad-os/samples/pictures).
It ships as a core package in images built with `LAZYOS_PICTURES=1`
(`python tools/run_demo.py --pictures`, the launcher's *Picture Viewer*).

## What it does

| | |
|---|---|
| Formats | PNG, JPEG, BMP (1/4/8/16/24/32-bit, uncompressed or bitfields), GIF (first frame) |
| Opening | Double-click a picture in Files (`mimed` maps `.png`, `.jpg`, `.jpeg`, `.bmp`, `.gif`); **Open...** anywhere; from the menu, the sample pictures (the wallpapers) |
| Paging | **Previous** / **Next**, arrow keys, Page Up/Down, Space, Backspace, the wheel, Home/End; wraps around the folder, sorted by name ignoring case |
| Sizing | **Best Fit** (shrink to the window, never enlarge), **Actual Size** (one picture pixel per screen pixel), **Zoom In** / **Zoom Out** and Ctrl+wheel through the classic steps (1% to 6400%), double-click to switch fit and 100% |
| Moving | Drag a picture larger than the window; it stops at its edges |
| Turning | **Rotate Right** / **Rotate Left** (`K` / `L`) |
| Slide show | **Slide Show** or F11: the next picture every four seconds; Escape stops |
| Edit | **Edit** (`E`) hands the picture to its editor through `mimed` (Paint for a PNG) |
| Status line | Name, size and format, position in the folder, zoom, rotation; a damaged picture is named here instead of shown |

Like XP's, turning a picture only changes the view: the viewer never writes
the file (it may read the picture's folder, not change it).

## How it is built

The LazyRAD side lives in [va1erian/lazyrad](https://github.com/va1erian/lazyrad)
(the pin in `lazyrad-os/Cargo.toml`):

* the **`PictureBox`** control (`xui-form/src/picture/`): decoding (PNG and
  JPEG by `xui-core`, BMP and GIF in the crate; a picture's size is read from
  its header and anything above 32 megapixels is refused before a pixel
  buffer is allocated), fit/zoom/pan geometry, rotation, `to_png`, and the
  `KeyDown`, `Wheel`, `Click` and `DoubleClick` events;
* **`Value::Bytes`**: a script reads a file with `file_read_bytes` (through
  the app's sandbox) and hands the blob to `picture1.load(...)`; the control
  never opens a file;
* **`open_file_dialog(title, filter, callback, "folder")`**: the picked
  file's folder is granted too, so Previous/Next can list it;
* **`app.documents`**: the files the player was started to open
  (`Platform::documents`).

The LazyOS side:

* `lrplay` tells a project from a document on its command line
  (`lazyrad-os/src/args.rs`: a directory or `.lrp` is the project, any other
  path a document, at most 64) and reports each as `LRPLAY:DOCUMENT:PASS:<path>`;
* the sandbox (`lazyrad-os/src/policy.rs`) grants each document and its
  folder read-only, beside the app's private data and its own project;
* `tools/xui/core_packages.py` packs `lrplay` as `bin/pictures.elf` with the
  project in `resources/project` and the wallpapers in `resources/project/samples`
  (the layout File -> Make LazyOS App gives a LazyRAD app);
* `build_support/pictures_embed.rs` embeds it for `LAZYOS_PICTURES=1`
  (refused without `LAZYOS_DESKTOP=1`), and the manifest registers it for
  `open`/`view` of the four picture types; `edit` stays with Paint.

## Verifying it

```bash
cd lazyrad-os && cargo test --test pictures      # the real project offscreen: paging, zoom, rotation, slide show, snapshots
python tools/xui/test_core_packages.py           # the package carries the player, the project and the samples
cargo test -p build-support-tests pictures       # the switch needs the desktop
python tools/lazyrad/build.py && python tools/xui/build.py
LAZYOS_DESKTOP=1 LAZYOS_PICTURES=1 LAZYOS_UI_PROBE=1 LAZYOS_XUI_AUTOSTART=term LAZYOS_RESET_OS=1 cargo build
python tools/screenshot/qemu_session.py --image target/lazyos.img \
    --out shots/pictures --script tools/screenshot/examples/lazyrad_pictures.json
```

The offscreen test writes `lazyrad-os/target/snapshots/pictures.png` (and a
rotated 2x render) to look at. The session copies two wallpapers to the home
folder, opens one through `mimed` as Files would, pages, rotates, zooms with
the toolbar and runs the slide show.
