# Quake

Quake on the [quake-srp](https://github.com/terrapapagalli1516/quake-srp)
engine — id Software's 1996 *Quake* ported to Rust (GPL-2.0) from the
WinQuake source, with the original 2-D layer, menus, console and sound —
playing the **freely redistributable shareware** (id's 1.06 pak, its licence
`SLICNSE.TXT` and the original `quake106.zip` travel in the package). The
plan and its status are in [`docs/quake-port-plan.md`](../docs/quake-port-plan.md).

## Build, install, play

```bash
python tools/quake/build.py        # engine + quake.elf + target/pkg/quake.lzp
python tools/run_demo.py --quake   # the desktop with /system/share/samples/quake.lzp
```

Quake is a user package, not a core one. In the desktop Terminal, copy it to
your home and install the copy, as a user installs anything
(`cp /system/share/samples/quake.lzp ~/ && pkgctl install ~/quake.lzp`; the
ramfs `/transient` is too small for the pak), or open the copy in Files: the
Installer shows the consent screen. Quake then appears in the start menu
with the other installed apps. The OS volume keeps installed apps across
boots; `run_demo.py --reset-os` starts over.

`tools/quake/build.py` needs the musl Rust target and zig (`pip install
ziglang==0.16.0`, the same toolchain as the Docs app, `tools/xui/zig.py`),
and network access the first time: it fetches quake-srp at the revision
pinned in `tools/quake/qfetch.py` and id's `quake106.zip` (the pak and the
archive, each checked against a pinned SHA-256), both cached under
`target/quake/`. Nothing is committed.

## How it fits together

| Piece | Where |
|---|---|
| Engine (Rust, unmodified) | `target/quake/quake-srp-<rev>/quake-rs/`, fetched, a path dependency of the assembled crate |
| Browser platform layer | `quake-wasm/`, same tree; its modules compile on LazyOS unchanged |
| Assembled crate | `target/quake/quake-srp-<rev>/lazyos/`: `quake-wasm` + this tree's overlay (`quake/src/`) |
| Ported root | `quake/src/main.rs`: quake-wasm's `mod` list, the startup, the data directory |
| The one patched upstream file | `quake/src/common.rs` (documented diff: the package names its data directory for the saves and `config.cfg`) |
| LazyOS bridge | `quake/src/lazy/`: `window.rs` + `window_input.rs` (the `xuid` window, `inputd` keys), `bridge.rs` (the record protocol: keys in, frames out), `records.rs` (the encoders), `launch.rs` (the command line, the paths), `keymap.rs`, `pixels.rs` (the 4:3 box), `headless.rs`, `crc.rs`, `launch/session.rs` (the grab, the key-state page) |
| Headless mode | `quake.elf -headless -frames 200`: `QUAKE:HEADLESS:PASS frames=N crc=<hex>`, verdict in `/tmp/quake-result.txt` |
| Package tree | `package/` (manifest, icons, docs); the build adds `bin/quake.elf`, `resources/id1/pak0.pak` and the licence files |
| Image switch | `LAZYOS_QUAKE=1` -> `build_support/quake_embed.rs` puts `/system/share/samples/quake.lzp` on the OS volume |

The game opens `id1/pak0.pak` from its install directory (`-basedir
<install>/resources`, found from `argv[0]` the way `init` spawns a packaged
binary). Saves and `config.cfg` go to `$HOME/.apps/org.lazy.quake` (the one
write the manifest asks for; `init` starts an installed app with its
session's `HOME`). Only with no `HOME` at all do they go to `/tmp/quake`,
which a reboot clears.

The launcher asks for the **Classic preset** (`-preset classic`, id's 1996
game: 320x200-shaped pictures shown in the 4:3 box, id's bindings). The
console `preset slop` switches a session to the 2026 look; its native
pictures draw square and fill the window.

## Checks

```bash
python tools/quake/build.py --test   # the upstream suite (record protocol, census) + the port's own, host
# an image built with LAZYOS_DESKTOP=1 LAZYOS_QUAKE=1 LAZYOS_XUI_AUTOSTART=term LAZYOS_UI_PROBE=1
python tools/screenshot/qemu_session.py --image target/lazyos.img --out shots/quake \
    --script tools/screenshot/examples/quake.json
```

`quake.json` installs the package through `pkgd`, runs
`quake.elf -headless -frames 200` headless (the deterministic verdict), then
launches the game from the start menu, waits for the first frame and plays a
few seconds of E1M1 from the attract loop.

Serial markers: `QUAKE:UP:PASS mode=window|headless,preset=classic`,
`QUAKE:PAK:FAIL`, `QUAKE:WINDOW:FAIL`, `QUAKE:DATA:FAIL`, `QUAKE:KEYSTATE:PASS`,
`QUAKE:GRAB:ON|OFF`, `QUAKE:FRAME:n:crc`, `QUAKE:HEADLESS:PASS`, `QUAKE:QUIT:PASS`.

## Controls

id's 1996 binds, on `inputd`'s sessions: arrows move and turn, Ctrl fires,
Space jumps, `[` and `]` walk the inventory, Shift runs, Alt strafes,
 `,`/`.` strafe, digits pick weapons, `Esc` is the menu, `~` the console,
F1 saves, F3 loads. Keyboard-only v1 (no mouse look: the compositor's
pointer is not handed to clients yet, the same limitation the Doom port
started with). Sound: the engine mixes (the attract demo's sounds ride the
bridge as `Pcm` records) but v1 routes none of them to `audiod` — the same
deferral the Doom port shipped (its plan's D6).

## Licences

See `NOTICE`: the engine is GPL-2.0-or-later (quake-srp, from id's GPL
source), this layer GPL-3.0-or-later (so the binary is GPL-3.0), the
shareware pak id's own terms (`resources/SLICNSE.TXT`, section 6: free
distribution as a whole, which this package keeps intact beside the game).
