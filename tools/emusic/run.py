#!/usr/bin/env python3
"""Build, boot, record and judge emusic's sound on LazyOS (docs/media-plan.md P4).

1. ``tools/emusic/build.py`` builds ``target/pkg/emusic.lzp``;
2. the desktop image is built with it, a sound card and a fresh OS volume;
3. ``tools/screenshot/examples/emusic_sound.json`` installs the package and
   runs ``emusic.elf --sound-check`` on its sample track while QEMU records
   the virtio-sound card to ``<out>/emusic.wav``;
4. ``tools/emusic/judge.py`` judges the recording (tones, timing, seek,
   pause, volume).

With ``--app`` it also runs ``emusic.json`` (the app opening the track through
``mimed`` and playing it to the end) and judges that recording: A4 for 2 s,
then C5 for 2 s.

    python tools/emusic/run.py                          # everything
    python tools/emusic/run.py --emusic-src ../emusic   # a local emusic clone
    python tools/emusic/run.py --no-build               # judge the current image again
    python tools/emusic/run.py --app                    # the app session too

Prints ``EMUSIC:RUN:PASS`` and exits 0 when every judged run passes. The
verdict is the recording (AGENTS.md: sound is never judged from markers).
"""

from __future__ import annotations

import argparse
import os
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
sys.path.insert(0, str(HERE))
import judge  # noqa: E402

PY = sys.executable
EXAMPLES = ROOT / "tools" / "screenshot" / "examples"
IMAGE_ENV = {
    "LAZYOS_DESKTOP": "1", "LAZYOS_EMUSIC": "1", "LAZYOS_SOUND": "1", "LAZYOS_DEVD": "1",
    "LAZYOS_XUI_AUTOSTART": "term", "LAZYOS_UI_PROBE": "1", "LAZYOS_RESET_OS": "1",
}


def step(argv: list[str], env: dict | None = None) -> bool:
    print("+", " ".join(argv), flush=True)
    return subprocess.run(argv, cwd=ROOT, env=env).returncode == 0


def build(emusic_src: Path | None, xui: bool) -> bool:
    package = [PY, "tools/emusic/build.py", "--require"]
    if emusic_src:
        package += ["--emusic-src", str(emusic_src)]
    if not step(package):
        return False
    if xui and not step([PY, "tools/xui/build.py"]):
        return False
    return image()


def image() -> bool:
    """The image with a fresh OS volume, so every session installs anew."""
    return step(["cargo", "build"], {**os.environ, **IMAGE_ENV})


def session(script: str, out: Path, args: argparse.Namespace) -> Path | None:
    """Run `script` while recording the sound card; the WAV, or None."""
    out.mkdir(parents=True, exist_ok=True)
    wav = out / "emusic.wav"
    argv = [PY, "tools/screenshot/qemu_session.py", "--image", "target/lazyos.img",
            "--out", str(out), "--script", str(EXAMPLES / script),
            "--extra-arg=-audiodev", f"--extra-arg=wav,id=a0,path={wav.as_posix()}",
            "--extra-arg=-device", "--extra-arg=virtio-sound-pci,audiodev=a0"]
    if args.accel:
        argv += ["--accel", args.accel]
    return wav if step(argv) and wav.is_file() else None


def judge_app(wav: Path) -> list[str]:
    """The app played the whole track once: A4 2 s, then C5 2 s."""
    recording = judge.read_wav(wav.read_bytes())
    found = judge.segments(recording.samples, recording.rate)
    expected = judge.EXPECTED[:2]
    if len(found) != 2:
        return [f"app: expected 2 tone segments, found {len(found)}"]
    return [f"app: {s.freq_hz:.0f} Hz/{s.duration_ms:.0f} ms, expected {e.freq} Hz/{e.ms} ms"
            for s, e in zip(found, expected)
            if abs(s.freq_hz - e.freq) > e.freq * judge.FREQ_TOLERANCE
            or abs(s.duration_ms - e.ms) > judge.DURATION_TOLERANCE_MS]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--no-build", action="store_true", help="use the current image")
    parser.add_argument("--no-xui", action="store_true",
                        help="skip tools/xui/build.py (the core apps are built already)")
    parser.add_argument("--emusic-src", type=Path, help="a local emusic clone")
    parser.add_argument("--app", action="store_true", help="also run and judge emusic.json")
    parser.add_argument("--accel", help="QEMU accelerator (default: auto)")
    parser.add_argument("--out", type=Path, default=ROOT / "shots", help="shots root")
    args = parser.parse_args()

    if not args.no_build and not build(args.emusic_src, not args.no_xui):
        print("EMUSIC:RUN:FAIL:build")
        return 1
    failures = []
    wav = session("emusic_sound.json", args.out / "emusic_sound", args)
    if wav is None:
        failures.append("the sound-check session failed")
    else:
        failures += judge.judge_wav(wav.read_bytes())[1]
    if args.app:
        # A second session needs a fresh volume: the first installed the package.
        if not image():
            failures.append("rebuilding the image failed")
        else:
            wav = session("emusic.json", args.out / "emusic", args)
            failures += ["the app session failed"] if wav is None else judge_app(wav)
    for failure in failures:
        print(f"EMUSIC:RUN:FAIL:{failure}")
    if failures:
        return 1
    print("EMUSIC:RUN:PASS")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
