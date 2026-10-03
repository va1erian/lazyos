# Doom for LazyOS

Doom on the [doomgeneric](https://github.com/ozkl/doomgeneric) engine, playing
Freedoom: Phase 1, shipped as the installable package `org.lazy.doom`
(`docs/packages.md`). The plan and its status are in
[`docs/doom-port-plan.md`](../docs/doom-port-plan.md).

## Build, install, play

```bash
python tools/doom/build.py        # engine + doom.elf + target/pkg/doom.lzp
python tools/run_demo.py --doom   # the desktop with /system/share/samples/doom.lzp
```

Doom is a user package, not a core one. In the desktop Terminal, copy it to
your home and install the copy, as a user installs anything
(`cp /system/share/samples/doom.lzp ~/ && pkgctl install ~/doom.lzp`; the
ramfs `/transient` is too small for its 10 MiB), or open the copy in Files:
the Installer shows the consent screen. Doom then appears in the start
menu with the other installed apps. The OS volume keeps installed apps across
boots; `run_demo.py --reset-os` starts over.

`tools/doom/build.py` needs the musl Rust target and zig (`pip install
ziglang==0.16.0`, the same toolchain as the Docs app, `tools/xui/zig.py`), and
network access the first time: it fetches doomgeneric at the revision pinned in
`tools/doom/fetch.py` and Freedoom's release zip, each checked against a pinned
SHA-256 and cached under `target/doom/`. Neither is ever committed.

## How it fits together

| Piece | Where |
|---|---|
| Engine (C, unmodified) | fetched to `target/doom/doomgeneric-<rev>/`, compiled by `build.rs` with `zig cc` |
| Platform hooks `DG_*` | `src/hooks.rs` |
| Window, frames, keys | `src/window.rs` on `xui_app::client_window` (buffer slots, `Present`, `inputd` session) |
| Headless mode | `src/headless.rs` |
| Pure logic, host-tested | `src/{keymap,keys,pixels,launch,crc}.rs` (`cargo test --lib` here) |
| Package tree | `package/` (manifest, icons, docs); the build adds `bin/doom.elf`, `resources/freedoom1.wad` and the licence files |
| Image switch | `LAZYOS_DOOM=1` -> `build_support/doom_embed.rs` puts `/system/share/samples/doom.lzp` on the OS volume |

The game finds its IWAD from `argv[0]` (`init` starts it by its absolute path
in the install directory): `<install>/resources/freedoom1.wad`. Config and
saves go to the package's own folder in the player's home,
`$HOME/.apps/org.lazy.doom` (the one write the manifest asks for; `init` starts
an installed app with its session's `HOME`). Only with no `HOME` at all do they
go to `/tmp/doom`, which a reboot clears.

## Checks

```bash
cargo test --manifest-path doom/Cargo.toml --lib    # keymap, key edges, scaling, args, CRC
python tools/screenshot/qemu_session.py --image target/lazyos.img --out shots/doom \
    --script tools/screenshot/examples/doom.json     # install, headless, menu launch, play
```

`doom.json` installs the package through `pkgd`, runs
`doom.elf -frames 200 -timedemo demo1` headless (it writes
`DOOM:HEADLESS:PASS frames=200 crc=<hex>`: a deterministic frame of the demo),
launches the game from the start menu, and plays a few seconds of E1M1.

Serial markers: `DOOM:UP:PASS mode=window|headless`, `DOOM:IWAD:FAIL`,
`DOOM:WINDOW:FAIL`, `DOOM:HEADLESS:PASS`, `DOOM:QUIT:PASS`.

## Controls

Arrows move, Ctrl fires, Space uses, Shift runs, Alt strafes, `,`/`.` strafe,
digits pick weapons, Esc is the menu. Without `inputd` (legacy compositor keys,
which never report a modifier alone) `F` fires and `R` runs. Sound, music and
mouse look are not implemented (plan D6).

## Licences

See `NOTICE`: the engine is GPL-2.0-or-later, this layer GPL-3.0-or-later (so
the binary is GPL-3.0), Freedoom is BSD-3-Clause.
