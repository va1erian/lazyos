# Games

Companion to [`AGENTS.md`](../../AGENTS.md); Lazygolf, the nes player, doom, emusic, quake and srb2kart.

## LazyGolf (`os.lazy.golf`)

`xui-golf` is a core desktop app, shipped only by `LAZYOS_GAMES=1` images
(`run_demo.py --games`, the launcher's Games group; the NES Player ships with
it, and the sessions below need that switch): a procedural 18-hole course generator and
a mid-90s indexed-colour software renderer to fly over, both in the portable
crate `xui-app/crates/golf` (`xui-golf`; the design is
[`docs/golf-course-generator.md`](../golf-course-generator.md)). The bin
only launches it; the crate's `golf` example runs the same app as a desktop
window through xui's winit backend. The game opens on the shot camera at the
first tee: aim with Left/Right, club with Up/Down, **Space** is the swing
meter's three clicks (start, power at the tick, accuracy on the line; `X`
cancels), PageUp / E and PageDown / Q tilt the view, `F` is the free camera the session below flies. Gameplay (ball
flight, bounce and roll, putting, caddy, flight cameras) is
[`docs/golf-gameplay-plan.md`](../golf-gameplay-plan.md), in
`xui-app/crates/golf/src/play/`; host tests `--test play --test shot
--test camera --test round` (the last plays holes through the real state
machine). Playing markers: `GOLF:SHOT`, `GOLF:LAND`, `GOLF:PENALTY`,
`GOLF:HOLED`; session `tools/screenshot/examples/xui_golf_play.json`. Session:
`tools/screenshot/examples/xui_golf.json` (`LAZYOS_XUI_AUTOSTART=golf`;
markers `GOLF:UP:PASS`, `GOLF:READY:<seed>:par=<par>:ms=<ms>:<name>`,
`GOLF:FPS:<fps>:scale=<n>:<w>x<h>:work=<ms>` once a second while drawing,
`GOLF:BENCH:frames=<n>:min=<fps>:avg=<fps>` after the `B` flyover).

```bash
cargo test --manifest-path xui-app/Cargo.toml -p xui-golf   # generator, renderer, window (snapshots in xui-app/target/snapshots)
cargo test --manifest-path xui-app/Cargo.toml -p xui-golf --test generator routing_survey -- --ignored --nocapture
cargo test --manifest-path xui-app/Cargo.toml -p xui-golf --test generator quality_survey -- --ignored --nocapture   # plain seed vs search
cargo test --manifest-path xui-app/Cargo.toml -p xui-golf --test render searched_views -- --ignored             # golf-search-*.png
cargo run --release --manifest-path xui-app/Cargo.toml -p xui-golf --example golf --features desktop -- [seed]
```

## NES Player (`os.lazy.nes`)

The NES Player is a core desktop app, shipped only by `LAZYOS_GAMES=1` images
(`run_demo.py --games`, with LazyGolf; `tools/nes/run.py` sets it): `xui-app/crates/nes` (`xui-nes`) holds
the session, the picture, the keyboard, the files and the window over
TetaNES's `tetanes-core` (MIT OR Apache-2.0: CPU, PPU, APU, 30+ mappers,
save states), and `xui-app/src/bin/nes.rs` supplies the LazyOS platform (a
48 kHz mono `audioclient` stream, `$HOME`, the `NES:*` evidence lines and
`--headless`). The image ships one cartridge, an original NROM homebrew
assembled by `tools/nes/` (a small 6502 assembler and a generator — no cc65
dependency): a title screen, a ball the D-pad moves and a four-note melody.
It is validated in a second emulator (jsnes, via Node). See
[`docs/nes-player.md`](../nes-player.md) and [`tools/nes/README.md`](../../tools/nes/README.md).

```bash
python tools/run_demo.py --games --sound   # then open NES from the menu
cargo test --manifest-path xui-app/Cargo.toml -p xui-nes   # session, window, pacer, scaler
python tools/nes/test_nesasm.py            # the assembler and the sample ROM
python tools/nes/make_sample.py --check    # the checked-in sample is current
python tools/nes/validate_external.py      # the ROM in jsnes (node/npm; skips without)
python tools/nes/run.py                    # build, boot, play, record, judge
```

Markers (channel `NES`): `UP`, `LOAD`, `RUN`, `SAVE`, `STATE`, `SOUND`,
`AUDIO`, `HEADLESS`, `QUIT`, and `FAIL`. `nes.elf --headless <rom>
[--frames N]` prints `NES:HEADLESS:PASS frames=N crc=<hex>` (also in
`/tmp/nes-result.txt`).

## Doom (an installable `.lzp` package)

Doom is `games/doom/` (doomgeneric, fetched at a pinned revision and compiled with
zig, plus a Rust platform layer on `xui-app`'s client window) shipped as the
package `org.lazy.doom` with the Freedoom IWAD inside; see
[`games/doom/README.md`](../../games/doom/README.md) and [`docs/doom-port-plan.md`](../doom-port-plan.md).

```bash
python tools/doom/build.py          # target/doom/doom.elf + target/pkg/doom.lzp (fetches doomgeneric, Freedoom)
python tools/run_demo.py --doom     # desktop with /system/share/samples/doom.lzp (a user package)
cargo test --manifest-path games/doom/Cargo.toml --lib
python tools/screenshot/qemu_session.py --image target/lazyos.img --out shots/doom     --script tools/screenshot/examples/doom.json   # needs a fresh OS volume (LAZYOS_RESET_OS=1) and LAZYOS_UI_PROBE=1
```

The Terminal reports one `TERM:OUT` per command, and a command that wraps past
80 columns reports its own tail instead: keep typed commands short (`doom.json`
sets `PS1='# '` first). Shell command substitution (`$(...)`) works since #518
(`cmdsubst_console.json`, `cmdsubst_desktop.json`).

## emusic (an installable `.lzp` package, `LAZYOS_EMUSIC=1`)

emusic (va1erian/emusic, the music player and library) runs as the package
`org.lazy.emusic`: `emusic/` is the LazyOS half (window through
`LazyOSBackend`, sound through `audiod`), emusic's own crates, fetched at the
revision `emusic/Cargo.toml` pins, are the rest (MP3s decoded by its
`emusic-lazyaudio` backend, symphonia, no BASS). See
[`docs/media-plan.md`](../media-plan.md) and [`emusic/README.md`](../../emusic/README.md).

```bash
python tools/emusic/build.py              # target/emusic/emusic.elf + target/pkg/emusic.lzp (--emusic-src DIR: a local clone)
python tools/run_demo.py --emusic         # desktop + /system/share/samples/emusic.lzp + a sound card
python tools/emusic/run.py --app          # build, install, record `emusic.elf --sound-check` and the app; judge the WAVs
python tools/emusic/test_judge.py         # the judge fails when it should
python tools/emusic/session.py --check    # emusic.json / emusic_sound.json are current
cargo test --manifest-path emusic/Cargo.toml --lib
```

The sessions need a fresh OS volume (`LAZYOS_RESET_OS=1`; `run.py` rebuilds
before each) and `LAZYOS_UI_PROBE=1`. Markers: `EMUSIC:UP:PASS`,
`EMUSIC:PLAY:<file>`, `EMUSIC:POS:<s>`, `EMUSIC:END:<file>`; the sound check
prints `EMUSIC:CHECK:DONE` and logs to `~/.apps/org.lazy.emusic/sound-check.log`.
QEMU's WAV recorder drops the time no stream plays, so the judge splits the
recording by pitch, not by silence.

## Quake (an installable `.lzp` package)

Quake is `games/quake/` (the `quake-srp` port of id's WinQuake to Rust, fetched at
a pinned revision like doomgeneric, plus a Rust platform layer that hosts
its browser platform over the same record protocol on `xui-app`'s client
window) shipped as the package `org.lazy.quake` **with id's freely
redistributable shareware pak inside**; see [`games/quake/README.md`](../../games/quake/README.md)
and [`docs/quake-port-plan.md`](../quake-port-plan.md). Only two upstream
files are overlaid (`src/main.rs` and `src/common.rs`'s documented data
directory); everything else is the upstream code.

```bash
python tools/quake/build.py            # target/quake/quake.elf + target/pkg/quake.lzp (+ id's quake106.zip; zig only links)
python tools/quake/build.py --test     # the upstream suite (record protocol, census) + the port's own, Linux host
python tools/quake/build.py --require  # a missing download is an error (CI)
python tools/run_demo.py --quake       # desktop with /system/share/samples/quake.lzp (a user package)
cargo test --manifest-path <assembled>/Cargo.toml # host; `--test` assembles and points at it
python tools/screenshot/qemu_session.py --image target/lazyos.img --out shots/quake \
    --script tools/screenshot/examples/quake.json   # needs a fresh OS volume (LAZYOS_RESET_OS=1) and LAZYOS_UI_PROBE=1
```

The launcher asks for the **Classic preset** (`-preset classic`, id's 1996
game in the 4:3 box); `preset slop` in the console switches a session to the
2026 look. The headless check (`quake.elf -headless -frames 300`) prints the
deterministic `QUAKE:HEADLESS:PASS frames=N crc=<hex>` (also in
`/tmp/quake-result.txt`); v1 is silent and pointer-free like Doom's (the
`Pcm` records already ride the bridge).

## SRB2Kart (an installable `.lzp` package, data fetched at build time)

SRB2Kart is `games/srb2kart/` (Kart-Public v1.6, Kart Krew's kart racer, fetched at
a pinned revision and compiled with zig, plus a C platform backend and a Rust
layer on `xui-app`'s client window) shipped as the package `org.lazy.srb2kart`
with **the engine only**: its game data (`srb2.srb`, `*.kart`, ~220 MB) is
Kart Krew's, unlicensed, so the image build downloads it from their own v1.6
release (SHA-256 pinned in `tools/srb2kart/fetch.py`) and writes it to
`/system/share/srb2kart/` (`fhs::share::SRB2KART_DATA`) beside a
`CREDITS.txt`. **Never commit, upload, attach or publish the data or an image
containing it** (CI artifacts carry screenshots and logs, no image); keep the
credits wherever the game appears. See [`games/srb2kart/README.md`](../../games/srb2kart/README.md)
and [`docs/srb2kart-port-plan.md`](../srb2kart-port-plan.md).

```bash
python tools/srb2kart/build.py            # engine + srb2kart.elf + target/pkg/srb2kart.lzp + target/srb2kart/data/
python tools/run_demo.py --srb2kart       # desktop + the package + the data; the OS volume defaults to 1G (--reset-os for an older image)
cargo test -p build-support-tests         # srb2kart_embed paths, the 1G default
python tools/screenshot/qemu_session.py --image target/lazyos.img --out shots/srb2kart \
    --script tools/screenshot/examples/srb2kart.json   # needs LAZYOS_SRB2KART=1, LAZYOS_RESET_OS=1 and LAZYOS_UI_PROBE=1
```

Markers (channel `SRB2KART`): `SRB2KART:UP:PASS mode=window|headless`,
`SRB2KART:DATA:FAIL`, `SRB2KART:HEADLESS:PASS frames=N crc=<hex>` (also in
`/tmp/srb2kart-result.txt`), `SRB2KART:QUIT:PASS`. v1 is silent (K2 adds sound).
