#!/usr/bin/env python3
"""Writes the NES Player's sample ROM.

    python tools/nes/make_sample.py            # write it
    python tools/nes/make_sample.py --check    # fail if the checked-in one is stale

`assets/samples/lazyos-sample.nes` ships in every image at
`/system/share/samples/`, where the app's welcome screen plays it;
`xui-app/crates/nes/tests/data/sample.nes` is the copy the crate's tests run.
Both are the same bytes, assembled by `tools/nes/sample_rom.py` with
`tools/nes/nesasm.py`. The build is deterministic (no timestamps, no host
input), so `--check` compares bytes and also validates the iNES structure, so
a corrupt file is named even when the generator changed on purpose.
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import sample_rom  # noqa: E402

ROOT = HERE.parent.parent
ASSET = ROOT / "assets" / "samples" / "lazyos-sample.nes"
FIXTURE = ROOT / "xui-app" / "crates" / "nes" / "tests" / "data" / "sample.nes"
HEADER = 16
PRG = 32 * 1024
CHR = 8 * 1024


def check(data: bytes) -> str | None:
    """Why `data` is not the sample ROM, or `None` when it is well formed."""
    if len(data) != HEADER + PRG + CHR:
        return f"is {len(data)} bytes, not {HEADER + PRG + CHR}"
    if data[:4] != b"NES\x1a":
        return "has no iNES magic"
    if (data[4], data[5]) != (2, 1):
        return "is not 32 KiB PRG + 8 KiB CHR"
    if data[6] != 0x02 or any(data[7:16]):
        return "is not mapper 0 with horizontal mirroring and a battery"
    prg = data[HEADER : HEADER + PRG]
    vectors = prg[PRG - 6 :]
    nmi = int.from_bytes(vectors[0:2], "little")
    reset = int.from_bytes(vectors[2:4], "little")
    irq = int.from_bytes(vectors[4:6], "little")
    if not 0x8000 <= reset <= 0xFFFF:
        return f"has a reset vector outside the cartridge ({reset:#06x})"
    if not 0x8000 <= nmi <= 0xFFFF:
        return f"has an NMI vector outside the cartridge ({nmi:#06x})"
    if irq != 0:
        return "has a non-zero IRQ vector"
    if prg[0] != 0x78:
        return "does not start with sei"
    chr_rom = data[HEADER + PRG :]
    if not any(chr_rom[:0x1000]):
        return "has an empty background pattern table"
    if not any(chr_rom[0x1000:]):
        return "has an empty sprite pattern table"
    return None


def write(path: Path, data: bytes) -> bool:
    """Writes `data` to `path` unless it is already there; whether it wrote."""
    if path.exists() and path.read_bytes() == data:
        return False
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(data)
    return True


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--check", action="store_true",
                        help="only verify the checked-in copies are current")
    args = parser.parse_args(argv)
    try:
        data = sample_rom.build()
    except Exception as error:  # noqa: BLE001 - report any assembler failure
        print(f"make_sample: the ROM did not build: {error}", file=sys.stderr)
        return 1
    if (reason := check(data)) is not None:
        print(f"make_sample: the built ROM {reason}", file=sys.stderr)
        return 1
    status = 0
    for path in (ASSET, FIXTURE):
        if args.check:
            if not path.exists():
                print(f"make_sample: {path} is missing; run without --check",
                      file=sys.stderr)
                status = 1
                continue
            current = path.read_bytes()
            if current != data:
                reason = check(current)
                detail = f" ({reason})" if reason else ""
                print(f"make_sample: {path} is stale{detail}; run without --check",
                      file=sys.stderr)
                status = 1
        elif write(path, data):
            print(f"wrote {path.relative_to(ROOT)}")
        else:
            print(f"{path.relative_to(ROOT)} is current")
    return status


if __name__ == "__main__":
    raise SystemExit(main())
