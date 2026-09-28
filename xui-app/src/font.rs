//! The bundled font, compiled into the app.
//!
//! LazyOS has no system font store, and its Linux ABI implements anonymous
//! `mmap` only, so the shaper cannot memory-map a font file even if one were
//! written to disk. The backend hands these bytes to the shaper directly.

/// JetBrains Mono Regular, the same face the kernel console and `xuid` use.
pub const BYTES: &[u8] = include_bytes!("../../assets/fonts/JetBrainsMono-Regular.ttf");
