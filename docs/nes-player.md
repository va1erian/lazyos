# The NES Player (`os.lazy.nes`)

An NES emulator with sound, shipped as a core desktop app. The emulation is
[TetaNES](https://github.com/lukexor/tetanes)'s `tetanes-core` crate (MIT OR
Apache-2.0): the 6502, the PPU, the APU's five channels, 30+ mappers, battery
RAM and save states. Everything around it — the session, the picture, the
window, the mixer integration and the files — is this repository's own work.

| Piece | Where |
|---|---|
| The emulator session: ROM loading (size cap, iNES/NES 2.0 header), battery saves, four save-state slots, reset/power cycle | `xui-app/crates/nes/src/session.rs` |
| The frame clock: wall clock and the mixer's ring together decide how many frames are due | `pacer.rs` |
| The picture: nearest-neighbour scaling, the NES's 8:7 pixels, letterboxing | `video.rs` |
| The keyboard: arrows/Z/X/A/S/Q/W/Enter, turbo, fast-forward | `input.rs` |
| The screen control: clocks the console, feeds the mixer, paints the picture and the paused overlay | `screen.rs` |
| The window: menus, welcome screen, status bar, dialogs, settings | `app.rs`, `ui.rs`, `settings.rs` |
| The LazyOS platform: an `audioclient` stream, `$HOME`, the sample cartridge, the `NES:*` lines, `--headless` | `xui-app/src/bin/nes.rs` |
| The sample cartridge: an original NROM homebrew | `tools/nes/sample_rom.py` |
| A small 6502 assembler (no cc65/asm6 dependency) | `tools/nes/nesasm.py` |
| The harness: build, boot, record, judge | `tools/nes/run.py`, `judge.py` |

## Running it

```bash
python tools/run_demo.py --desktop --sound    # then open NES from the menu
python tools/run_demo.py --desktop --sound --xui-autostart nes   # start it at login
python tools/xui/build.py                     # just the apps
```

The app is a core package: `pkgd` installs it at boot and the start menu shows
it. Opening a `.nes` file in Files launches it on that cartridge (`mimed`
maps the extension); a cartridge dropped on the window opens too
(`text/uri-list`). The welcome screen offers the sample the image ships,
`/system/share/samples/lazyos-sample.nes`.

## The sample cartridge

No Nintendo ROM is shipped. `tools/nes/` adds a small 6502 assembler
(`nesasm.py`) and an original NROM homebrew (`sample_rom.py`): a title screen
with a hand-drawn 8x8 font, a ball the D-pad moves, a four-note melody (A4, C5,
E5, A5) on pulse 1 and a C6 beep on A. It stores the controller byte at `$10`
and a frame counter at `$12`/`$13`, so tests can read the emulated console's
own view of input and time; `make_sample.py` writes it to
`assets/samples/lazyos-sample.nes` (shipped at `/system/share/samples/`) and to
the crate's test data.

Two things the cartridge taught the tests, both now asserted. The ball is
drawn by four sprites, and the PPU copies `OAM[OAMADDR & 0xF8 ..]` over the
first eight OAM bytes on the pre-render line when `OAMADDR >= 8`, so a write
loop that leaves `OAMADDR` at 16 loses the two top sprites every frame. The
session test measures the ball's extent (16x15 pixels) and the jsnes check
does the same, so neither the ROM nor an emulator can regress it silently.

## The frame clock and the sound

The console must run at 60.0988 Hz and the mixer drains at its own rate, so
neither alone can pace the emulator. Each tick of the screen control's timer
asks [`pacer`](../xui-app/crates/nes/src/pacer.rs) how many display frames are
due: the wall clock bounds it (never more than three frames in one call, so a
stall does not become a burst), and the mixer's ring bounds it too (a frame
whose samples would not fit is not run). The frame's `f32` mix goes through a
gain of 5 (`audio.rs`) into the stream: `tetanes-core` reproduces the NES's
non-linear mixer, which keeps even a full mix inside ±0.2 — faithful, and far
quieter than the rest of the system.

## Files

`$HOME/.apps/os.lazy.nes`:

* `saves/<stem>.sram` — battery-backed cartridge RAM, written on unload, every
  30 s while playing and on quit;
* `states/<stem>/slot-<n>.state` — four save-state slots;
* `config.txt` — scale, aspect, NTSC filter, FPS counter, slot, mute;
* `recent.txt` — the last few cartridges.

The stem is sanitised (letters, digits, `-`, `_`, `.`; nothing else), so a
crafted file name cannot name a file outside the directory. No cartridge is
shipped except the sample, which is this repository's own work (see
`assets/manifest.txt`).

## Evidence and the harness

Markers on the `NES` channel: `NES:UP:PASS` (first frame),
`NES:LOAD:PASS:<path>:mapper=..:prg=..:chr=..:battery=..:region=..`,
`NES:RUN:PASS:frames=<n>`, `NES:SAVE:PASS:<path>`,
`NES:STATE:PASS:save|load=<slot>:<path>`, `NES:SOUND:PASS:<rate>:<channels>`,
`NES:AUDIO:PASS:<rate>:<channels>`, `NES:QUIT:PASS`, and `NES:FAIL:<what>` for
anything refused. `nes.elf --headless <rom> [--frames N]` runs the console
with no window and prints `NES:HEADLESS:PASS frames=<n> crc=<hex>` (also in
`/tmp/nes-result.txt`): the deterministic check, like Doom's `-frames`.

```bash
python tools/nes/test_nesasm.py            # the assembler and the sample ROM
python tools/nes/test_judge.py             # the judge fails when it should
python tools/nes/make_sample.py --check    # the checked-in sample is current
cargo test --manifest-path xui-app/Cargo.toml -p xui-nes
python tools/nes/run.py                    # build, boot, play, record, judge
python tools/nes/run.py --accel auto       # WHPX/KVM when the host has it
```

`run.py` builds a desktop image with the Terminal, the UI probe and the sound
card, launches the app on the sample cartridge from the Terminal, watches the
console run, holds the D-pad so the ball moves, saves and loads a state, pauses
and quits, and runs the headless frame checksum, while QEMU records the sound
card to a WAV. The verdicts are the serial markers, the recording (the sample's
melody: A4, C5, E5, A5), and the screenshots (`shot_01_playing.png` shows the
sample ROM's title screen inside the window, `shot_02_moved.png` the ball
moved, `shot_03_paused.png` the pause overlay).

Host tests need no QEMU: the session runs the sample ROM and asserts the
picture, the melody's pitch, the controller byte the ROM reads back, a save
state round-trip and the battery file; `tests/window.rs` drives the real
window offscreen (menus, reports, snapshots in `xui-app/target/snapshots/`).

## The cartridge is validated against a second emulator

The sample ROM could be right for one emulator's quirks and wrong on real
hardware, so `tools/nes/validate_external.py` runs the same file in
[jsnes](https://github.com/bfirsh/jsnes) (Apache-2.0: an independent CPU, PPU
and APU, in JavaScript, installed by npm into `target/` and never shipped).
The ROM stores what a checker wants to know about the console it is running
on — the controller byte it reads at `$10`, a frame counter at `$12`/`$13` —
so the check is about the ROM's own view, not about pixels:

```bash
python tools/nes/validate_external.py     # NES:EXTERNAL:PASS ... (skips without node/npm)
```

It must see the ROM count its frames, read `$01` back while Right is held,
move the ball, draw more than one colour with a dominant backdrop, and sound
its first note at 440 Hz. The palettes differ between emulators (jsnes's index
1 is red, TetaNES's is dark blue), so the picture check is structural rather
than pixel-exact; the pitch and the counters are not.

## Not there yet

* No gamepad (the keyboard only; `inputd` reports gamepads, a later phase).
* No Zapper, no four-player adapter, no rewind.
* A save state restores the console, but the PPU's frame buffer is derived
  state: the picture is blank for the first frame after a load.
* The NTSC filter is offered but the default is straight pixels; the filter's
  cost on the guest has not been measured.

## Licences

`tetanes-core` is MIT OR Apache-2.0 (taken under MIT, per `THIRD_PARTY.md`'s
rule); the app, the harness and the sample cartridge are this repository's
work (GPL-3.0-or-later). NES is a trademark of Nintendo; the app is not
affiliated with or endorsed by Nintendo, and ships no cartridge but its own.
