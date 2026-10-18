#!/usr/bin/env python3
"""Validate the sample cartridge against a second, independent NES emulator.

The image ships one cartridge, assembled by `tools/nes/sample_rom.py`; the
NES Player runs it and `tools/nes/run.py` judges that run from the serial log
and a recording. This check answers the other half of the question: is the
*ROM* right, or does it only work because of one emulator's quirks?

[jsnes](https://github.com/bfirsh/jsnes) (Apache-2.0, a JavaScript NES
emulator with its own CPU, PPU and APU) runs the same file under Node and
reports what the console did: the ROM's own frame counter, the byte it reads
back from a held D-pad, the picture's colours and the first note's pitch.
`tools/nes/jsnes_check.js` is the Node half.

    python tools/nes/validate_external.py            # install jsnes if needed, check
    python tools/nes/validate_external.py --require  # a missing toolchain is an error

jsnes is installed by npm into ``target/nes-jsnes`` — never committed, never
in the image (the check is a host tool; `docs/nes-player.md`).
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
ROM = ROOT / "assets" / "samples" / "lazyos-sample.nes"
SCRIPT = HERE / "jsnes_check.js"
#: Where npm installs jsnes for this check, and the version it installs: the
#: check's API assumptions are jsnes 2.1.0's, so the pin is exact.
JSNES_DIR = ROOT / "target" / "nes-jsnes"
JSNES_VERSION = "2.1.0"

#: The console's counters must reach at least these values in `--frames`.
MIN_FRAMES = 150
MIN_FRAMES_AFTER_NOTE = 25
#: The picture: at least this many distinct colours, with the backdrop
#: covering most of the screen. The palettes differ between emulators, so the
#: check is structural, not pixel-exact.
MIN_COLOURS = 3
MIN_BACKDROP_SHARE = 0.5
#: The ball's sprite colour must span its whole 16x15 shape (a ROM that leaves
#: OAMADDR at 16 loses its two top sprites to the pre-render OAM copy).
MIN_SPRITE_ROWS = 14
MIN_SPRITE_COLUMNS = 15
#: The ROM's first note (A4) and how far the measurement may be off.
NOTE_HZ = 440.0
NOTE_TOLERANCE = 0.06
#: The recording must not be silence.
MIN_PEAK = 0.01
MIN_SAMPLES = 10_000


def judge(report: dict, frames: int) -> list[str]:
    """Failures in what jsnes reported, empty when the cartridge is sound."""
    failures = []
    if report.get("frames", 0) < MIN_FRAMES:
        failures.append(
            f"the ROM counted {report.get('frames')} frames, wanted {MIN_FRAMES}"
        )
    if report.get("frames_after_note", 0) < MIN_FRAMES_AFTER_NOTE:
        failures.append(
            f"the ROM counted {report.get('frames_after_note')} frames in the first "
            f"{frames} display frames, wanted {MIN_FRAMES_AFTER_NOTE}"
        )
    if report.get("joy", 0) & 0x01 != 0x01:
        failures.append(
            f"the ROM read back ${report.get('joy', 0):02X} with Right held (bit 0 unset)"
        )
    if not report.get("moved"):
        failures.append("the ball did not move while Right was held")
    if report.get("colours", 0) < MIN_COLOURS:
        failures.append(f"the picture has {report.get('colours')} colours")
    share = report.get("common_pixels", 0) / max(1, report.get("total_pixels", 1))
    if share < MIN_BACKDROP_SHARE:
        failures.append(f"the most common colour covers {share:.0%} of the screen")
    if report.get("sprite_rows", 0) < MIN_SPRITE_ROWS or report.get(
        "sprite_columns", 0
    ) < MIN_SPRITE_COLUMNS:
        failures.append(
            "the ball is "
            f"{report.get('sprite_columns')}x{report.get('sprite_rows')}, not 16x15: "
            "its top sprites were eaten (leave OAMADDR at 0 after the OAM writes)"
        )
    hz = report.get("note_hz") or 0
    if abs(hz - NOTE_HZ) > NOTE_HZ * NOTE_TOLERANCE:
        failures.append(f"the first note sounds at {hz} Hz, not {NOTE_HZ:g} Hz")
    if report.get("audio_samples", 0) < MIN_SAMPLES:
        failures.append(f"only {report.get('audio_samples')} samples of audio")
    if report.get("peak", 0) < MIN_PEAK:
        failures.append(f"the audio is silent (peak {report.get('peak')})")
    return failures


def have(tool: str) -> bool:
    """Whether `tool` is on the PATH."""
    return shutil.which(tool) is not None


def install() -> None:
    """Installs jsnes into [`JSNES_DIR`] when it is not there (pinned)."""
    package = JSNES_DIR / "node_modules" / "jsnes" / "package.json"
    if package.exists():
        installed = json.loads(package.read_text(encoding="utf-8")).get("version")
        if installed == JSNES_VERSION:
            return
        print(f"jsnes {installed} is not {JSNES_VERSION}; reinstalling")
        shutil.rmtree(JSNES_DIR / "node_modules" / "jsnes", ignore_errors=True)
    JSNES_DIR.mkdir(parents=True, exist_ok=True)
    print(f"installing jsnes@{JSNES_VERSION} into {JSNES_DIR} (npm)")
    subprocess.run(
        [
            "npm",
            "install",
            "--silent",
            "--no-audit",
            "--no-fund",
            f"jsnes@{JSNES_VERSION}",
        ],
        cwd=JSNES_DIR,
        check=True,
    )


def run(rom: Path, frames: int) -> dict:
    """Runs the ROM in jsnes and returns what the Node script reported."""
    env = dict(os.environ, JSNES_DIR=str(JSNES_DIR))
    done = subprocess.run(
        ["node", str(SCRIPT), str(rom), str(frames)],
        cwd=ROOT,
        env=env,
        check=True,
        capture_output=True,
        text=True,
    )
    return json.loads(done.stdout.strip().splitlines()[-1])


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--rom", type=Path, default=ROM)
    parser.add_argument("--frames", type=int, default=200)
    parser.add_argument(
        "--require",
        action="store_true",
        help="a missing node/npm is an error rather than a skip",
    )
    args = parser.parse_args(argv)
    if not (have("node") and have("npm")):
        print("NES:EXTERNAL:SKIP no node/npm on this host")
        return 1 if args.require else 0
    if not args.rom.is_file():
        print(f"NES:EXTERNAL:FAIL no cartridge at {args.rom}")
        return 1
    install()
    try:
        report = run(args.rom, args.frames)
    except (subprocess.CalledProcessError, json.JSONDecodeError) as error:
        print(f"NES:EXTERNAL:FAIL jsnes did not run: {error}")
        return 1
    failures = judge(report, args.frames)
    if failures:
        print("NES:EXTERNAL:FAIL")
        for failure in failures:
            print(f"  {failure}")
        print(f"  jsnes said: {json.dumps(report)}")
        return 1
    print(
        "NES:EXTERNAL:PASS"
        f" jsnes frames={report['frames']}"
        f" colours={report['colours']}"
        f" note={report['note_hz']}Hz"
        f" joy=${report['joy']:02X}"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
