# SRB2Kart on LazyOS

SRB2Kart (Kart Krew's kart racer, the Sonic Robo Blast 2 engine) as a LazyOS
desktop app. The plan is [`docs/srb2kart-port-plan.md`](../docs/srb2kart-port-plan.md);
this directory is the program: a standalone Cargo workspace (static musl, built
with zig like `doom/`) whose Rust `main` hosts the upstream C engine.

- The engine is **Kart-Public v1.6** (`https://github.com/STJr/Kart-Public`),
  fetched by `tools/srb2kart/fetch.py` at a SHA-256-pinned release archive and
  never vendored. The game data is the official `AssetsLinuxOnly.zip` of the
  same release, fetched the same way (the digest matches Flathub's
  `org.srb2.SRB2Kart` manifest). The data has no licence and is never
  committed, packaged or uploaded.
- `c/lazyos/` is our platform layer (`i_main.c`, `i_system.c`, `i_video.c`,
  `i_sound.c`, `i_net.c`, `i_cdmus.c`, `i_input.c`), compiled in place of
  upstream's `src/sdl/`. `lazyos_host.h` is its seam to Rust.
- `src/hooks.rs` implements that seam (`lz_*`) on a `backend::Backend`:
  `headless::Headless` (no display, checksummed frames, a virtual clock) now,
  the desktop window in K1. `src/launch.rs` parses the command line and picks
  the data and config directories; `src/engine.rs` starts the engine.

```bash
python tools/srb2kart/build.py --engine-only   # target/srb2kart/srb2kart.elf (+ the fetched source)
python tools/srb2kart/build.py                 # also the data in target/srb2kart/data and the package
cargo test --manifest-path srb2kart/Cargo.toml --lib   # host tests of the pure modules

# the Linux host (WSL) is the fast loop; the same ELF runs on LazyOS
target/srb2kart/srb2kart.elf -headless -frames 300 -data target/srb2kart/data -home /tmp/k
#   SRB2KART:HEADLESS:PASS frames=300 crc=<hex>      (also /tmp/srb2kart-result.txt)
# ~1200 frames reach the title screen; SRB2KART_DUMP=/tmp/f.ppm writes the last frame
```

`-headless` runs at 320x200 and on a virtual clock, so the checksum is the same
on every run and machine. Other arguments go to the engine (`-width`, `-height`,
`+map`, ...); the wrapper consumes `-headless`, `-frames N`, `-data DIR`,
`-home DIR`, `--client` and `attempt=N`.

## Deviations from the plan

No upstream file is patched and no overlay is applied. What upstream's build
assumes and we do instead:

- **Not `NONET`.** Upstream's `NONET` build does not compile (`i_tcp.c`'s ban
  list, `mserv.c`). The network code is built in and never started
  (`I_InitNetwork` says no) until K3.
- **Two shims** (`c/lazyos/mserv_shim.h`, `d_clisrv_shim.h`), force-included
  into `mserv.c` and `d_clisrv.c` only: both use names that exist only under
  `HAVE_CURL`, and the shims define them as inert values.
- **`JOYSTICK_HOTPLUG`** is defined on the command line (upstream gets it from
  `sdl/sdlmain.h`), and `i_input.c` defines the joystick console variables,
  `strupr`/`strlwr` and the master server entry points that upstream gets from
  its SDL and libcurl builds.
- **The engine's home is `<home>/.srb2kart`**, with `<home>` the app folder
  (`$HOME/.apps/org.lazy.srb2kart`, or `/tmp/srb2kart`): upstream always
  appends `DEFAULTDIR`, so `i_main.c` passes `-home <app folder>`.
- The wrapper's pure modules (`launch`, `crc`, `headless`, `backend`) are in
  the library so `cargo test --lib` runs on any host; `xui-app` and its
  siblings are dependencies of the musl target only.
