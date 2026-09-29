# Bundled fonts and credits

All fonts here are redistributed unmodified under permissive licenses.

| File | Font | Used by | License |
|---|---|---|---|
| `DroidSans.ttf` | Droid Sans (proportional sans-serif) | `xuid` chrome (titles, taskbar, menus); every xui app | Apache-2.0 |
| `DroidSerif-Regular.ttf` | Droid Serif (proportional serif) | `xuid` headings and placeholders (Alt+Tab title, "Waiting for buffer") | Apache-2.0 |
| `JetBrainsMono-Regular.ttf` | JetBrains Mono (monospace) | kernel framebuffer console; the xui Terminal | SIL OFL 1.1 |

## Credits

- **Droid Sans** — Copyright 2007 Google Corporation; designed by Steve Matteson
  of Ascender Corporation. "Droid is a trademark of Google." Licensed under the
  Apache License 2.0 (`LICENSE-Apache-2.0.txt`, also
  <https://www.apache.org/licenses/LICENSE-2.0>).
- **Droid Serif** — Copyright 2012, 2013 Google Inc.; designed by the Monotype
  Imaging design team. Licensed under the Apache License 2.0
  (`LICENSE-Apache-2.0.txt`).
- **JetBrains Mono** — Copyright 2020 The JetBrains Mono Project Authors
  (<https://github.com/JetBrains/JetBrainsMono>), SIL OFL 1.1 (`OFL.txt`).

Droid Sans and Droid Serif were taken from the Android Open Source Project
(`frameworks/base/data/fonts`, tag `android-4.4_r1`, mirrored at
<https://github.com/aosp-mirror/platform_frameworks_base>). The files are
byte-for-byte as shipped there. Android's Droid family was succeeded by Noto
Sans / Noto Serif (OFL); Droid remains the redistributable Apache-2.0 release.

## How they are used

- `user/build.rs` rasterizes Droid Sans and Serif at 13 px with `font-atlas`
  into anti-aliased proportional glyph atlases (per-glyph advance in 1/16 px).
  `user/src/messenger/display/typeface.rs` exposes them as `display::Face` and
  `Canvas::text_face`; `xuid` draws all chrome text with it.
- `xui-app/src/font.rs` embeds `DroidSans.ttf` with `include_bytes!` and
  registers it with the `cosmic-text` shaper, so it is the default UI face of
  every xui app except the Terminal, which needs a fixed character grid and
  registers JetBrains Mono as its default family instead
  (`xui_canvas::add_font` / `set_default_family`, a vendored addition).
- The 5x7 bitmap font in `display::font` remains for the small demo clients
  (`xdemo`, `dragdemo`, `shellprobe`).
