# xui-canvas (vendored)

Upstream: <https://github.com/va1erian/xui> (`crates/xui-canvas`, rev
`2747818`), MIT.

This copy differs from upstream in three small ways:

1. **`set_default_font`** registers an in-memory font file for the cosmic-text
   shaper. Upstream builds its `FontSystem` from the directories `fontdb`
   scans, and fontdb memory-maps files, which LazyOS does not support (its
   Linux ABI has anonymous `mmap` only). The LazyOS backend loads the
   repository's bundled `JetBrainsMono-Regular.ttf` with `include_bytes!` and
   installs it through this hook, so no system font directory is involved.
2. **Horizontal text alignment.** Upstream passes `TextAlign` to cosmic-text's
   paragraph alignment, which only positions *wrapped* lines; a natural-width
   run draws from the rectangle's left edge whatever the style says. The
   viewers' right-aligned table cells need `TextAlign::Center`/`End`, so
   `text::draw` applies the alignment offset itself, per line.
3. **`Surface::pixels`.** Upstream only offers `to_image`, which clones the
   whole pixmap per call. The LazyOS backend presents a frame per timer tick,
   and a fresh screen-sized clone every frame exhausts LazyOS's bump-only
   `mmap` window, so the backend copies from a borrowed pixel slice instead.

`xui-app/Cargo.toml` keeps the pinned git dependency on the xui repository and
`[patch]`es `xui-canvas` onto this copy, so the rest of the graph (notably
`xui-core`) still resolves from git.
