# Quake to LazyOS: the plan

Status: **working** — the game runs the shareware from a windowed desktop
session, and the headless mode gives an exact-signature check.

The port is [`quake-srp`](https://github.com/terrapapagalli1516/quake-srp)
("the slop rust port"), id Software's *Quake* (1996) ported to Rust from the
WinQuake C source — the file formats, the QuakeC VM, the server, the
software renderer, the menus, the console and the mixer — in plain
`#![forbid(unsafe_code)]` Rust with no dependencies. It is the same
architecture the browser plays: a portable engine (`quake-rs`), a platform
layer (`quake-wasm`) whose interface is a small record protocol, and a host
(the browser page) that feeds events in and presents frames out. LazyOS
becomes that host; no engine change is needed.

What this port is:

- id's **freely redistributable shareware** (1.06) in the package: the
  pak comes out of the original `quake106.zip` (with its `SLICNSE.TXT`
  beside it and the archive itself, the same way `quake-srp`'s own demo
  distributes it), so the game runs out of the box;
- the **desktop app** (`org.lazy.quake`, a user package like Doom's):
  `xui` window, `inputd` key sessions, the keyboard grab while maximized;
- a **headless check** (`quake.elf -headless -frames N`) whose verdict
  line is a deterministic signature of frames of the attract demo —
  the CI-visible proof the whole stack (the ABI shim, the protocol
  bridge, the engine) still runs.

Like Doom's port
([`docs/doom-port-plan.md`](docs/doom-port-plan.md), which this plan
mirrors), the engine is never committed: `tools/quake/qfetch.py` fetches
`quake-srp` at a pinned revision and id's zip at pinned SHA-256s; nothing
new is checked into `target/`.

## Decisions

- **D1: the platform layer is `quake-wasm`'s, not a new one.**
  quake-srp's browser build is the portable host: saves as `std::fs`
  files, `config.cfg`, the menu/console command table over
  `std::fs::stdin/stdout` records. The protocol has no browser things in
  it (keys, ticks, windows, PCM), so LazyOS runs the *same* code with an
  in-process pair of pipes (`quake/src/lazy/bridge.rs`): the program's
  loop parks on its `Read` while the window pumps keys; each turn's
  `Frame` record is scaled onto the `xuid` surface. This keeps ~1
  MLoC of platform code identical to upstream (the upstream tests,
  including the census and the `quaketool play` frame hashes, run
  unchanged against it on a Linux host).
- **D2: the engine is fetched, never vendored; the overlay is the port.**
  `tools/quake/build.py` assembles
  `target/quake/quake-srp-<rev>/lazyos/`: the fetched `quake-wasm/`
  sources plus `quake/src/` (the port's root and platform) — exactly two
  upstream files are replaced (`main.rs` wholesale, and `common.rs`
  with one documented addition, the data directory), and `src/lazy/` is
  the LazyOS half. The overlay's fingerprint rebuilds the assembled
  crate incrementally.
- **D3: the search path is the package's own resources.**
  `-basedir <install>/resources` (found from `argv[0]`, the Doom port's
  rule), so `id1/pak0.pak` is read where the manifest put it. The saves
  and `config.cfg` go to `$HOME/.apps/org.lazy.quake` — the one write the
  manifest asks for — through `common.rs`'s data directory; with no
  `$HOME` they go to the ramfs `/tmp/quake`.
- **D4: Classic is the default preset.** The launcher sets `-preset
  classic` (id's 1996 game: id's modes in the 4:3 box, id's bindings and
  `320x200`-shaped frames at whole-pixel steps), which also keeps the
  deterministic headless signature independent of the window. A session
  can `preset slop` (the 2026 look: native, square pixels, the frame at
  the window's pixel size) — v1 displays it by filling the window with
  the picture's own ratio; the Classic look keeps the status bar's
  1.2x-tall scaling exactly (`vid.rs`'s rule).
- **D5: a user package, not a core one.** As with Doom: `LAZYOS_QUAKE=1`
  embeds `/system/share/samples/quake.lzp` only; a user installs a copy
  from their home (`pkgctl install`, or Files). The shareware licence
  asks exactly that kind of free whole distribution, and 18 MiB is too
  much for a default image.
- **D6: v1 is silent and pointer-free.** The engine still mixes and hands
  `Pcm` records over; routing them through `libs/audioclient` into
  `audiod` is a small follow-up of the same shape Doom's D6 left. Mouse
  look waits on the platforms' pointer story (the Doom port's same
  limitation); the game is fully playable on id's keyboard binds, and a
  maximized window holds the keyboard grab so every key reaches it.
- **D7: the deterministic headless check.** Headless injects the
  automation call `set_resolution 320 200` and then fixed `1/72`-second
  ticks (the pace `bench.py` and the census pin). The attract demo's
  frame at tick N is then machine-independent, which is what the `crc`
  signature says. Without `-frames` the budget is 300 frames (the
  attract loop never ends by itself and the harness must terminate).

## Pieces

| Piece | Where |
|---|---|
| Fetcher (pinned rev + pak) | `tools/quake/qfetch.py` |
| Build (assemble, zig cc, package) | `tools/quake/build.py` |
| Overlay crate template | `quake/Cargo.toml.in` (generated into the assembled crate with the workspace's absolute paths) |
| Port root | `quake/src/main.rs` (quake-wasm's module list + our `main`) |
| Patched upstream file | `quake/src/common.rs` (the data directory) |
| LazyOS platform | `quake/src/lazy/` (`launch`, `keymap`, `session`, `pixels`, `records`, `headless`, `crc`, `bridge`, `window`, `window_input`) |
| Image switch | `LAZYOS_QUAKE=1` -> `build_support/quake_embed.rs` |
| Launcher + GUI | `run_demo.py --quake`; GUI Simple/Advanced through `tools/lazygui/` (`catalog.py`, `appsteps.py`, `simple.py`, `simplecfg.py`, `ui.py`, `variables.py`) |
| Package tree | `quake/package/` (manifest, icons, docs) |
| Session | `tools/screenshot/examples/quake.json` (install, headless verdict, menu launch, play) |
| CI | `.github/workflows/quake.yml` (build, `--test` the crate on Linux, the session) |

## Status

- [x] Engine builds with zig cc, static musl (2.1 MiB ELF).
- [x] Headless verdict (`QUAKE:HEADLESS:PASS frames=N crc=<hex>`,
      `/tmp/quake-result.txt`) — deterministic across runs (the WSL run
      repeated the same crc 3/3 at 300 frames).
- [x] Package (`quake.lzp`) with the shareware pak, licence and archive;
      image embed (`LAZYOS_QUAKE=1`, `run_demo.py --quake`, GUI Simple +
      Advanced).
- [x] Window mode on the desktop session (`quake.json`'s launch + play
      steps; the first frame is the attract demo's title view).
- [ ] Sound through `audiod` (the `Pcm` records are already there;
      `libs/audioclient`'s `PlaybackStream` at the engine's rate, D6).
- [ ] Mouse look (as with Doom's D6; pointer events for clients are the
      platform-level prerequisite).
- [ ] The registered game and mission packs, from the player's own
      `pak1.pak` beside the shareware one (the engine already layer it;
      the package's resources just need the files, D3).

## Checks

```bash
python tools/quake/build.py --test      # the upstream suite + the port's own, on a Linux host
python tools/quake/build.py --require   # engine + package (CI exits 1 when a download fails)
python tools/run_demo.py --quake        # desktop with /system/share/samples/quake.lzp
python tools/screenshot/qemu_session.py --image target/lazyos.img --out shots/quake \
    --script tools/screenshot/examples/quake.json   # LAZYOS_UI_PROBE=1 image, fresh OS volume
```

Serial markers: `QUAKE:UP:PASS mode=window|headless,preset=classic`,
`QUAKE:PAK:FAIL`, `QUAKE:WINDOW:FAIL`, `QUAKE:DATA:FAIL`,
`QUAKE:KEYSTATE:PASS|FAIL`, `QUAKE:GRAB:ON|OFF|FAIL`,
`QUAKE:FRAME:n:crc`, `QUAKE:HEADLESS:PASS frames=N crc=<hex>`,
`QUAKE:QUIT:PASS`.
