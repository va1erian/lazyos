#!/usr/bin/env python3
"""Judge the Volume applet by ear (docs/tray-plan.md T3).

`tools/screenshot/examples/tray_applets.json` plays a 440 Hz tone, turns the
master volume down from 100 % to 50 % with ten wheel notches over the Volume
icon, then plays a 660 Hz tone, while QEMU records the sound card to a WAV
(`-audiodev wav`). The verdict is the recording, as for the sound harness
(`tools/sound`): the second tone must be heard at about half the level of the
first.

    python tools/tray/volume_check.py shots/tray_applets/volume.wav
    python tools/tray/volume_check.py rec.wav --before 440 --after 660 --ratio 0.5
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "sound"))
import analyze_wav  # noqa: E402

#: How far a tone's frequency may be from the asked one.
FREQ_TOLERANCE = 0.05
#: How far the measured level ratio may be from the expected one.
RATIO_TOLERANCE = 0.15


def tone_rms(segments: list[analyze_wav.Segment], freq: float) -> float | None:
    """The RMS level of the longest segment at `freq`."""
    matching = [s for s in segments
                if s.freq_hz is not None and abs(s.freq_hz - freq) <= FREQ_TOLERANCE * freq]
    if not matching:
        return None
    return max(matching, key=lambda s: s.duration_ms).rms


def judge(wav: analyze_wav.Wav, before: float, after: float, ratio: float) -> tuple[bool, str]:
    """Whether the `after` tone is `ratio` times as loud as the `before` one."""
    segments = analyze_wav.find_segments(wav)
    first, second = tone_rms(segments, before), tone_rms(segments, after)
    if first is None or second is None:
        found = ", ".join(f"{s.freq_hz:.0f} Hz" for s in segments if s.freq_hz) or "none"
        return False, f"missing a tone (heard: {found})"
    measured = second / first
    ok = abs(measured - ratio) <= RATIO_TOLERANCE * ratio
    return ok, f"{before:.0f} Hz rms={first:.0f}, {after:.0f} Hz rms={second:.0f}, ratio={measured:.2f} (want {ratio:.2f})"


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("wav", type=Path)
    parser.add_argument("--before", type=float, default=440.0)
    parser.add_argument("--after", type=float, default=660.0)
    parser.add_argument("--ratio", type=float, default=0.5)
    args = parser.parse_args(argv)
    try:
        wav = analyze_wav.read_wav(args.wav.read_bytes())
    except (OSError, analyze_wav.WavError) as error:
        print(f"TRAY:VOLUME:FAIL {error}")
        return 1
    ok, detail = judge(wav, args.before, args.after, args.ratio)
    print(f"TRAY:VOLUME:{'PASS' if ok else 'FAIL'} {detail}")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
