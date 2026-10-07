# Bundled fonts and credits

All fonts here are redistributed unmodified under permissive licenses.

| File | Font | Used by | License |
|---|---|---|---|
| `DroidSans.ttf` | Droid Sans (proportional sans-serif) | `xuid` chrome (titles, taskbar, menus); every xui app | Apache-2.0 |
| `DroidSans-Bold.ttf` | Droid Sans Bold | the Docs app (headings, emphasis); LazyWriter's bold | Apache-2.0 |
| `DroidSerif-Regular.ttf` | Droid Serif (proportional serif) | `xuid` headings and placeholders (Alt+Tab title, "Waiting for buffer"); LazyWriter's Serif family | Apache-2.0 |
| `JetBrainsMono-Regular.ttf` | JetBrains Mono (monospace) | kernel framebuffer console; the xui Terminal | SIL OFL 1.1 |
| `liberation/Liberation{Sans,Serif,Mono}-{Regular,Bold,Italic,BoldItalic}.ttf` | Liberation Sans, Serif and Mono 2.1.5 | LazyWeb's web pages (installed to `/system/share/fonts/liberation` in desktop images) | SIL OFL 1.1 |

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
- **Liberation Sans, Serif and Mono** — Digitized data copyright (c) 2010
  Google Corporation with Reserved Font Arimo, Tinos and Cousine; copyright
  (c) 2012 Red Hat, Inc. with Reserved Font Name Liberation. SIL OFL 1.1
  (`liberation/LICENSE`, authors in `liberation/AUTHORS`). Release 2.1.5,
  `liberation-fonts-ttf-2.1.5.tar.gz` from
  <https://github.com/liberationfonts/liberation-fonts> (sha256
  `7191c669bf38899f73a2094ed00f7b800553364f90e2637010a69c0e268f25d0`),
  unmodified.

Droid Sans, Droid Sans Bold and Droid Serif were taken from the Android Open Source Project
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
  LazyWriter (`font::register_writer`) also registers Droid Sans Bold, Droid
  Serif and JetBrains Mono for its Sans, Serif and Mono families; with no
  italic face, its italic is slanted from the regular one by the shaper.
- LazyWeb (`xui-app/web/src/fonts.rs`) reads the twelve Liberation faces from
  `/system/share/fonts/liberation` at start and draws pages with them: they
  are metric-compatible with Arial, Times New Roman and Courier New, and
  have real bold and italic faces. Its window keeps Droid Sans. A family
  whose regular face is missing falls back to the Droid one.
- The 5x7 bitmap font in `display::font` remains for the small demo clients
  (`xdemo`, `dragdemo`, `shellprobe`).
