# xui-canvas (vendored)

Upstream: <https://github.com/va1erian/xui> (`crates/xui-canvas`, rev
`2747818`), MIT.

This copy differs from upstream by exactly one addition: `set_default_font`,
which registers an in-memory font file for the cosmic-text shaper. Upstream
builds its `FontSystem` from the directories `fontdb` scans, and fontdb
memory-maps files, which LazyOS does not support (its Linux ABI has anonymous
`mmap` only). The LazyOS backend loads the repository's bundled
`JetBrainsMono-Regular.ttf` with `include_bytes!` and installs it through this
hook, so no system font directory is involved.

`xui-app/Cargo.toml` keeps the pinned git dependency on the xui repository and
`[patch]`es `xui-canvas` onto this copy, so the rest of the graph (notably
`xui-core`) still resolves from git.
