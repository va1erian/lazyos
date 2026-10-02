# lazyrad build

`lazyrad-os/` is a standalone Rust workspace (bins `lrplay` and `lazyrad`) built
for `x86_64-unknown-linux-musl`, separate from the OS workspace. This script
builds it and copies the two static ELFs into `target/lazyrad/`, where the root
`build.rs` embeds them when `LAZYOS_LAZYRAD=1`:

```bash
python tools/lazyrad/build.py                 # both bins, release
python tools/lazyrad/build.py --bin lrplay    # just one
python tools/lazyrad/build.py --debug         # debug profile
```

The recipe mirrors `tools/rhai/build.py` and `tools/xui/build.py`. On Windows
the musl target has no host linker, so cargo is pointed at the toolchain's
bundled `rust-lld` through target-specific `CARGO_TARGET_*` variables; elsewhere
the default linker is used. The environment is set only for the cargo
subprocess.

A missing `lazyrad-os/` is an error (exit 1). A missing musl target or linker
is reported as a warning and the script exits 0 with an empty JSON map, so a
host without them skips the run instead of failing; a real compile error exits
1, and a build that does not produce an expected ELF exits 1 rather than
leaving a silently empty image.

## Embedding in the image

With `LAZYOS_LAZYRAD=1` set for the OS build, `build_support/lazyrad_embed.rs`
adds `/system/bin/lrplay` and `/system/bin/lazyrad` to the OS volume and copies
every sample project directory named by `LAZYRAD_SAMPLES` (a platform path list,
`;` on Windows and `:` elsewhere; relative entries resolve against the repo
root) under `/system/share/lazyrad/<directory>/`. Names are kept exactly (ext2 is case-sensitive). A missing ELF fails
the OS build with a message to run this script. With the switch unset nothing
changes.

## Tests

```bash
python tools/lazyrad/test_build.py
```

The tests exercise argument parsing, the Windows linker environment and the
output-path logic without invoking cargo.
