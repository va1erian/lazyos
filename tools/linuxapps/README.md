# Linux apps: real static musl programs for LazyOS

`build.py` fetches pinned upstream sources and builds unmodified Linux
command-line programs as static `x86_64-linux-musl` executables, to exercise
LazyOS's Linux ABI with real software.

```bash
python tools/linuxapps/build.py                # all five -> target/linuxapps/bin/<name>
python tools/linuxapps/build.py --only jq dash # some
python tools/linuxapps/build.py --require      # a missing toolchain/download fails
python tools/linuxapps/smoke.py                # run each once in alpine:3.20 (Docker)
python tools/linuxapps/test_build.py           # unit tests of the pure helpers
```

`build.py` prints `{name: path or null}` as JSON. Without zig, Rust's musl
target or network the affected entries are null and it exits 0 (1 with
`--require`); a compile error, or an output that is not an ELF64 x86-64
executable without `PT_INTERP`, exits 1.

| name | upstream | built with | notes |
|------|----------|-----------|-------|
| `lua` | Lua 5.4.7 | zig cc | `LUA_USE_POSIX`; no readline, no `dlopen` (static) |
| `sqlite3` | SQLite 3.46.1 amalgamation | zig cc | `SQLITE_THREADSAFE=0`, `SQLITE_OMIT_LOAD_EXTENSION`, no readline |
| `jq` | jq 1.7.1 release tarball | zig cc | configure's results as `-D` flags; decNumber on, no oniguruma (no regex) |
| `dash` | dash 0.5.12 | zig cc | hand-written `config.h` (no libedit); generators below |
| `rg` | ripgrep 14.1.1 crate | cargo + rust-lld | jemalloc removed (see `rg.py`); `--locked` |

## Toolchain and pins

* C: zig (`pip install ziglang==0.16.0`), found by `tools/xui/zig.py`;
  `zig cc -target x86_64-linux-musl -static -Os`, stripped (`toolchain.py`).
* Rust: the linker setup of `tools/rhai/build.py` (bundled `rust-lld`,
  self-contained musl on Windows), imported, not copied.
* Downloads: `tools/doom/fetch.py`'s `download`, cached in
  `target/linuxapps/src/`. Each archive is pinned by SHA-256 in `sources.py`,
  which also records how each digest was cross-checked against upstream.
  Builds work on a copy in `target/linuxapps/build/<name>`.

## dash's generated sources

`mkinit`, `mknodes` and `mksyntax` are compiled for the host with zig and run
unchanged. `mktokens` and `mkbuiltins` (sh + awk/sed/sort/nl/tr) and
`mksignames` (which prints the *build* machine's signal numbers) are
reimplemented in `dashgen.py`; `builtins.def` is preprocessed by zig's
target preprocessor first. Every generated file was checked byte-for-byte
against the upstream tools run in Alpine (only `signames.c`'s comment names
the program differently); `test_build.py` pins the details.

## ripgrep and jemalloc

On musl, ripgrep always links jemalloc, whose autoconf build cannot
cross-compile from Windows. `rg.py` fetches the published crate (pinned by
its crates.io checksum) and removes the dependency from `Cargo.toml`, the
`#[global_allocator]` from `crates/core/main.rs` and the two packages from
`Cargo.lock`, so it uses musl's malloc (upstream's choice everywhere but
musl) and `--locked` still pins every other dependency.

## On LazyOS

`LAZYOS_LINUXAPPS=1 cargo build` (or `python tools/run_demo.py --linuxapps`, or
the launcher's "Linux programs" checkbox, Simple or Advanced tab) places every
built program in `/system/bin`, where the shell finds it by name. They are
checked by the ABI bench (`python tools/abi/run.py --only dash,lua,sqlite3,jq,rg`,
one boot of the `linuxapps` fixture) and shown by two session scripts:

```bash
LAZYOS_CLI=1 LAZYOS_LINUXAPPS=1 cargo build
python tools/screenshot/qemu_session.py --image target/lazyos.img \
    --out shots/linuxapps_console --script tools/screenshot/examples/linuxapps_console.json
LAZYOS_DESKTOP=1 LAZYOS_LINUXAPPS=1 cargo build     # after tools/xui/build.py
python tools/screenshot/qemu_session.py --image target/lazyos.img \
    --out shots/linuxapps_desktop --script tools/screenshot/examples/linuxapps_desktop.json
```
