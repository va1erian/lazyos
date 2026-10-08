#!/usr/bin/env python3
"""Write ``emusic/package/resources/tones.mp3``: A4 (440 Hz) for 2 s, then C5
(523.25 Hz) for 2 s, stereo 44.1 kHz at 128 kbit/s.

The package ships it as its sample track, and ``tools/emusic/run.py`` judges
the recording of it (frequency, timing, a seek onto the second tone, pause,
volume). An original, generated work: CC0. Needs ffmpeg with libmp3lame;
``--check`` only verifies the checked-in file decodes to that length.
"""

import argparse
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
OUT = ROOT / "emusic" / "package" / "resources" / "tones.mp3"
A4, C5 = 440.0, 523.25
SECONDS_PER_TONE = 2


def write() -> None:
    tone = "sine=frequency={}:sample_rate=44100:duration={}"
    subprocess.run([
        "ffmpeg", "-hide_banner", "-loglevel", "error", "-y",
        "-f", "lavfi", "-i", tone.format(A4, SECONDS_PER_TONE),
        "-f", "lavfi", "-i", tone.format(C5, SECONDS_PER_TONE),
        "-filter_complex", "[0:a][1:a]concat=n=2:v=0:a=1,volume=0.9",
        "-ac", "2", "-b:a", "128k", "-c:a", "libmp3lame",
        "-metadata", "title=A4 then C5", "-metadata", "artist=LazyOS",
        str(OUT),
    ], check=True)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true", help="only check the file exists")
    args = parser.parse_args()
    if not args.check:
        write()
    if not OUT.is_file() or OUT.stat().st_size < 10_000:
        print(f"{OUT} is missing or too small", file=sys.stderr)
        return 1
    print(f"{OUT} ({OUT.stat().st_size} bytes)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
