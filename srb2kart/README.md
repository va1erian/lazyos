# SRB2Kart for LazyOS

SRB2Kart v1.6 (Kart Krew's kart racer on the Sonic Robo Blast 2 engine) as the
installable package `org.lazy.srb2kart` (`docs/packages.md`), on the software
renderer, keyboard-driven, offline. The plan, decisions and phases are in
[`docs/srb2kart-port-plan.md`](../docs/srb2kart-port-plan.md).

## Build, install, play

```bash
python tools/srb2kart/build.py        # engine + srb2kart.elf + target/pkg/srb2kart.lzp + target/srb2kart/data/
python tools/run_demo.py --srb2kart   # the desktop with the package and the data
```

The image holds the engine package at `/system/share/samples/srb2kart.lzp` and
the game data at `/system/share/srb2kart/` (`LAZYOS_SRB2KART=1`,
`build_support/srb2kart_embed.rs`). With that switch the OS volume defaults to
`1G` (`LAZYOS_OS_SIZE` still wins; an existing smaller volume needs
`run_demo.py --reset-os`). Like Doom it is a user package: in the Terminal,
`cp /system/share/samples/srb2kart.lzp ~/ && pkgctl install ~/srb2kart.lzp`,
then start SRB2Kart from the Games menu. In the launcher it comes with the
Simple tab's Games group, and the Advanced tab has its own checkbox.

## The data rule

The game data (`srb2.srb`, `*.kart`, about 220 MB) is Kart Krew's, has no
licence, and the characters are SEGA's. Therefore:

- It is **fetched at build time** from Kart Krew's own v1.6 release
  (`https://github.com/STJr/Kart-Public/releases/download/v1.6/AssetsLinuxOnly.zip`,
  pinned by SHA-256 in `tools/srb2kart/fetch.py`) into `target/srb2kart/data/`.
- **Never commit, upload, attach or publish the data, or a disk image that
  contains it.** CI may fetch it to run a session but uploads only screenshots
  and logs, never the image.
- The package (`.lzp`) carries the engine only, so it stays pure GPL.

## Credits

- SRB2Kart: **Kart Krew**; Sonic Robo Blast 2: **Sonic Team Junior**; the
  engine descends from **Doom Legacy** and **id Software**'s Doom source
  (GPL-2.0-or-later). Kart-Public: https://github.com/STJr/Kart-Public.
- The data is (c) Kart Krew and is shown by `/system/share/srb2kart/CREDITS.txt`
  (from [`CREDITS.txt`](CREDITS.txt)); the package's `NOTICE` and docs page
  say the same.
- Sonic the Hedgehog and related characters are trademarks of SEGA. SRB2Kart
  is a fan game, not affiliated with or endorsed by SEGA or Sonic Team.
- Our platform layer is GPL-3.0-or-later; see [`NOTICE`](NOTICE).

## Files in this directory

| Path | What |
|---|---|
| `package/` | manifest, icons, docs page; the build adds `bin/srb2kart.elf` and the licence files |
| `NOTICE`, `CREDITS.txt` | licences and the data's credits |
| `src/`, `c/lazyos/`, `build.rs`, `Cargo.toml` | the Rust program and the C platform backend (see the plan) |


## The program

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

## Checks

```bash
cargo test -p build-support-tests -p fhs                 # the embed paths and the 1G default
python tools/lazygui/test_catalog_srb2kart.py            # the launcher switches
# an image built with LAZYOS_DESKTOP=1 LAZYOS_SRB2KART=1 LAZYOS_XUI_AUTOSTART=term LAZYOS_UI_PROBE=1 LAZYOS_RESET_OS=1
python tools/screenshot/qemu_session.py --image target/lazyos.img --out shots/srb2kart \
    --script tools/screenshot/examples/srb2kart.json     # install, headless, menu launch, a race
```
