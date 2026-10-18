# tools/nes — the NES Player's sample ROM and harness

The NES Player (`os.lazy.nes`) is an xui app in `xui-app/`; these are the
host-side tools around it. See [`docs/nes-player.md`](../../docs/nes-player.md).

| File | What it is |
|---|---|
| `nesasm.py` | A small two-pass 6502 assembler: the official opcodes, labels, `.byte`/`.word`/`.res`/`.org`. The repo has no cc65/asm6 dependency, and the sample ROM must build reproducibly on any host. |
| `sample_rom.py` | The sample cartridge: an original NROM homebrew — a title screen with an 8x8 font drawn here, a 16x16 ball the D-pad moves, a four-note melody (A4, C5, E5, A5) on pulse 1 and a C6 beep on A. It stores the controller byte at `$10` and a frame counter at `$12`/`$13` (mirrored into battery-backed PRG RAM at `$6000`), so a test can read the emulated console's own view of input and time. |
| `make_sample.py` | Writes the ROM to `assets/samples/lazyos-sample.nes` (shipped at `/system/share/samples/`) and `xui-app/crates/nes/tests/data/sample.nes` (the crate's tests). `--check` fails when a checked-in copy is stale and validates the iNES structure. |
| `judge.py` | Judges a session: the serial markers, the recording (the melody's notes) and the screenshots. |
| `run.py` | Builds the apps and a desktop image with the sound card, boots QEMU recording the sound card to a WAV, runs `tools/screenshot/examples/xui_nes.json`, and judges everything. |

```bash
python tools/nes/test_nesasm.py            # the assembler and the ROM's structure
python tools/nes/test_judge.py             # the judge fails when it should
python tools/nes/make_sample.py            # write the sample
python tools/nes/make_sample.py --check    # the checked-in copies are current
python tools/nes/run.py                    # build, boot, play, record, judge
```

The ROM is this repository's own work (GPL-3.0-or-later, one line in
`assets/manifest.txt`); nothing Nintendo owns is shipped or fetched.
