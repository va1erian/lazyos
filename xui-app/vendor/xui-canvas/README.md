# xui-canvas (vendored)

Upstream: <https://github.com/va1erian/xui> (`crates/xui-canvas`, rev
`5efb730`), MIT.

This copy differs from upstream in two ways (issue #351):

**The windowed backend is dropped.** Upstream `xui-canvas` carries a
`WinitBackend` with `winit`/`softbuffer`/`glutin`/`glow`, a `GlWidget` GPU seam
over `xui-gpu`, and an `arboard` clipboard. LazyOS drives its own display
protocol (`os.lazy.display.v1`) and its Linux ABI cannot host `winit`
(anonymous `mmap` only), so those backends are unreachable here. They also pull
`wayland-sys`'s `dlib`, which emits `-ldl` and breaks the LazyOS musl link.
The `backend/`, `gl/`, `sys/` and `clipboard.rs` modules and their dependencies
are therefore removed; `backend/geometry.rs` moves to `geometry.rs` for the
offscreen walk, and the offscreen backend's GL-fallback hooks are removed with
them. Everything else (the portable `SkiaCanvas`/`Surface`, `image_cache`,
`text`, `text_layout`, `offscreen`, `snapshot`) is upstream verbatim.

The three LazyOS additions:

1. **`set_default_font` / `add_font` / `set_default_family`** register an
   in-memory font file for the cosmic-text shaper. Upstream builds its
   `FontSystem` from the directories `fontdb` scans, and fontdb memory-maps
   files, which LazyOS does not support (its Linux ABI has anonymous `mmap`
   only). The LazyOS backend loads the repository's bundled
   `JetBrainsMono-Regular.ttf` with `include_bytes!` and installs it through
   this hook, so no system font directory is involved. `set_default_family`
   makes runs whose `TextStyle`/`FontSpec` name no family use the bundled face
   (the Terminal's monospace).
2. **Horizontal text alignment.** Upstream passes `TextAlign` to cosmic-text's
   paragraph alignment, which only positions *wrapped* lines; a natural-width
   run draws from the rectangle's left edge whatever the style says. The
   viewers' right-aligned table cells need `TextAlign::Center`/`End`, so
   `text::draw` applies the alignment offset itself, per line. The shaper is given
   no paragraph alignment at all: cosmic-text 0.19 also aligns a non-wrapped
   line against the buffer width, which applied the offset twice (right-aligned
   Editor line numbers landed on top of the text).
3. **`Surface::pixels`.** Upstream only offers `to_image`, which clones the
   whole pixmap per call. The LazyOS backend presents a frame per timer tick,
   and a fresh screen-sized clone every frame exhausts LazyOS's bump-only
   `mmap` window, so the backend copies from a borrowed pixel slice instead.

`xui-app/Cargo.toml` keeps the pinned git dependency on the xui repository and
`[patch]`es `xui-canvas` onto this copy, so `xui-core` still resolves from git
at the same revision.
