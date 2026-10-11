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
then start SRB2Kart from the Games menu. The launcher's Simple tab has an
"SRB2Kart" option and the Advanced tab a checkbox.

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

Upstream files are never patched; a needed fix is an overlay copied over the
fetched tree by `tools/srb2kart/build.py` and listed here (none yet).

## Checks

```bash
cargo test -p build-support-tests -p fhs                 # the embed paths and the 1G default
python tools/lazygui/test_catalog_srb2kart.py            # the launcher switches
# an image built with LAZYOS_DESKTOP=1 LAZYOS_SRB2KART=1 LAZYOS_XUI_AUTOSTART=term LAZYOS_UI_PROBE=1 LAZYOS_RESET_OS=1
python tools/screenshot/qemu_session.py --image target/lazyos.img --out shots/srb2kart \
    --script tools/screenshot/examples/srb2kart.json     # install, headless, menu launch, a race
```
