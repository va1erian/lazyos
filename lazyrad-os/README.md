# lazyrad-os

LazyRAD on LazyOS (see [`docs/lazyrad-plan.md`](../docs/lazyrad-plan.md)): a
standalone Rust workspace, like `xui-app` and `rhai-host`, built for
`x86_64-unknown-linux-musl` by `python tools/lazyrad/build.py` and embedded in the
disk image as `LRPLAY.ELF` and `LAZYRAD.ELF` (`LAZYOS_LAZYRAD=1`).

| Bin | What |
|---|---|
| `lrplay` | The player: runs one LazyRAD project as a `xuid` client. It is also the stub copied into every `.lzp` package. |
| `lazyrad` | The IDE as a `xuid` client (plan P3). |

The library (`src/lib.rs`) holds the LazyOS glue shared by both: command-line
parsing (`args`), the `lazyrad_runtime::platform::Platform` LazyOS installs
(`platform`: script file sandbox, config dir, player path), the serial
evidence markers (`marker`: `LRPLAY:UP|EVENT|EXIT`, `LRIDE:*`) and Messenger
for form scripts (`messenger`). That module registers `rhai_lazy::msg` as a
LazyRAD script extension (LazyRAD `lazyrad_runtime::extensions`), so
`msg::connect("os.lazy.confd.v1").info()` works in any form script; see
[`docs/rhai/msg.md`](../docs/rhai/msg.md). The player prints `LRPLAY:MSG:PASS` once it is
installed.

## Running

```bash
python tools/xui/build.py && python tools/lazyrad/build.py
LAZYOS_DESKTOP=1 LAZYOS_XUI_APPS=target/xui/xui-term.elf LAZYOS_XUI_AUTOSTART=term \
LAZYOS_LAZYRAD=1 LAZYRAD_SAMPLES="<lazyrad>/examples/hello;<lazyrad>/examples/calculator" cargo build
python tools/screenshot/qemu_session.py --image target/lazyos.img --out shots/lrplay \
    --script tools/screenshot/examples/lazyrad_hello.json --fail-on "LRPLAY:[A-Z]+:FAIL"
```

In the Terminal: `/LRPLAY.ELF --client /LAZYRAD/hello &`. Command line:
`lrplay [--client] [--project <dir> | <dir>] [attempt=N]`; see `src/args.rs` for
how a relative `--project` and the default `resources/project` resolve (against
the install directory, located from `argv[0]`; `current_exe()` is `/busybox` on
LazyOS today).

## LazyRAD dependency and the rev pin

LazyRAD is a git dependency on `va1erian/lazyrad` with `default-features = false`
(no `winit`, `rfd`, `directories`, `dark-light`, local-time). The LazyRAD side of
this work (feature gating, the `Platform` trait, `run_with_backend`, the
packager) lives on LazyRAD's `lazyos-p0` branch.

**The pinned `rev` in `Cargo.toml` is LazyRAD `main` before that branch merged, so
`lazyrad-os` does not build from the pin alone.** Until the LazyRAD PR merges,
develop against a local checkout by appending a patch to the LazyOS repo's
`.cargo/config.toml` (cargo reads config from the working directory, which
`tools/lazyrad/build.py` sets to the repo root; do not commit it):

The `msg` integration also needs LazyRAD's `lazyrad_runtime::extensions`
(va1erian/lazyrad#82); point the patch at a checkout that has both.

```toml
[patch."https://github.com/va1erian/lazyrad"]
lazyrad-runtime  = { path = "<lazyrad>/crates/lazyrad-runtime" }
lazyrad-player   = { path = "<lazyrad>/crates/lazyrad-player" }
lazyrad-project  = { path = "<lazyrad>/crates/lazyrad-project" }
lazyrad-packager = { path = "<lazyrad>/crates/lazyrad-packager" }
xui-form         = { path = "<lazyrad>/crates/xui-form" }
xui-rhai         = { path = "<lazyrad>/crates/xui-rhai" }
```

**After the merge:** set `rev` on every `va1erian/lazyrad` line in `Cargo.toml` to
the merge commit, delete the patch, and commit `Cargo.lock`. Bumping xui is the
procedure in [`docs/xui-plan.md`](../docs/xui-plan.md): LazyRAD's workspace, this
`Cargo.toml` and `xui-app/Cargo.toml` must all name one xui revision (today
`58c1a6e`).

## Tests

```bash
cd lazyrad-os && cargo test    # args, platform, markers, and tests/lzp_conformance.rs
# LAZYRAD_SAMPLES=<lazyrad>/examples/hello;... adds the real samples to the conformance run
```
