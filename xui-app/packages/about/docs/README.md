# About LazyOS

What LazyOS is, in a few pages, over a real-time 3D scene: a chrome torus
knot (or torus, icosahedron, icosphere) above a synthwave floor, drawn by a
software rasteriser in pure Rust. Every pixel is computed on the CPU.

| Input | Does |
|-------|------|
| drag | spin the model (it keeps its momentum) |
| wheel, Up / Down | zoom |
| Space, Tab | next shape |
| W | solid, solid + wireframe, wireframe |
| Left / Right | previous / next page (pages also turn on their own) |
| Esc, Q | close |

The renderer and the window are the portable crate `xui-app/crates/about`
(`cargo test -p xui-about`; `cargo run --release -p xui-about --example about
--features desktop` runs the same window on a host). The window adapts its
internal resolution to the frame cost, so a slow machine draws chunkier
pixels instead of dropping frames.
