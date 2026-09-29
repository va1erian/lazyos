# xui-icons — Global Village

A multi-colour vector icon set for [xui](../xui-core) with a 90s world-music
look: chunky ink outlines, bevelled highlights and woven zig-zag trim. 36 icons
on a 32×32 grid, built for dark themes first.

| Category | Icons |
|---|---|
| Hardware | computer, monitor, keyboard, mouse, hard-disk, floppy, cd-rom, printer, modem, network, server, speaker |
| Files | folder, folder-open, document, image, music, archive |
| System | trash, trash-full, settings, search, home, clock, mail, lock, help, warning, info, error |
| Toolkit | rpc, pub-sub, theme, widget, terminal, window |

The crate is optional, depends on `xui-core` only and has no `unsafe`. Enable it
from the umbrella crate with the `icons` feature, or depend on it directly.

```rust
use xui_icons::{Palette, Village, draw};

// In a custom painter, with a `&mut dyn Canvas` and a `Rect`:
draw(canvas, Village::Network, rect, &Palette::GLOBAL_VILLAGE);
```

`draw` fills the smaller side of `rect`, centred, through
`Canvas::fill_path` and `Canvas::stroke_path`. That is the portable path API:
`xui-canvas` rasterises it with `tiny-skia` (software, or offscreen for
screenshots) and `xui-win32` with Direct2D, at any size and DPI.

## Colour and dark mode

Icons carry their own colours; each shape names a `Tone` (ink, cream, teal,
rose, amber, cobalt, clay, …) and a `Palette` maps tones to colours.
`Palette::GLOBAL_VILLAGE` is the default. The dark ink outline suits mid and
light surfaces; on a very dark one, retint it:

```rust
use xui_core::backend::Rgba;
use xui_icons::{Palette, Tone};

let on_black = Palette::GLOBAL_VILLAGE.with(Tone::Ink, Rgba::rgb(0x3A, 0x31, 0x90));
```

## Gallery

```sh
cargo run -p xui-icons --example gallery
```

renders `village-dark.png` and `village-light.png` into `target/snapshots/`
headlessly.

## Assets

```text
assets/svg/         source of truth: one standalone 32×32 SVG per icon
assets/png/<size>/  alpha PNG exports at 16, 24, 32, 48, 64, 128 and 256 px
assets/ico/         Windows .ico per icon (16, 24, 32, 48, 64 and 256 px)
assets/generate.py  compiles assets/svg into src/data/ (standard library only)
```

Edit an SVG, then run `python crates/xui-icons/assets/generate.py` and commit
the regenerated `src/data/`. Supported SVG: `rect`, `circle`, `ellipse` and
`path` (`M L H V C S A Z`, absolute or relative), styled with the classes in
each file's `<style>`; the PNG and `.ico` files are exports and are not read by
the build.
