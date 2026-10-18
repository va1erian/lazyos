#!/usr/bin/env python3
"""Build, boot, play and judge the NES Player (docs/nes-player.md).

Builds the desktop apps, then a desktop image with the Terminal, the UI probe
and the sound card, boots it with QEMU recording the sound card to a WAV
(``-audiodev wav``), runs ``tools/screenshot/examples/xui_nes.json`` -- the
Terminal launches the NES Player on the sample cartridge, the console runs,
the D-pad moves the ball, a save state round-trips, the app pauses and quits
-- and judges with ``judge.py``:

* the serial log (``shot_*``/``serial.log``): the LOAD/RUN/STATE/QUIT markers
  and no ``NES:FAIL``;
* the recording: the sample ROM's melody (A4, C5, E5, A5);
* the screenshots: the console's picture, the ball moved, the pause overlay.

Read the screenshots too: ``shot_01_playing.png`` must show the sample ROM's
title screen inside the window.

Usage::

    python tools/nes/run.py                # build, run, judge
    python tools/nes/run.py --no-build     # reuse target/lazyos.img
    python tools/nes/run.py --accel auto   # WHPX/KVM when the host has it
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
SCRIPT = ROOT / "tools" / "screenshot" / "examples" / "xui_nes.json"
OUT = ROOT / "shots" / "nes"
WAV = OUT / "nes.wav"


def run(argv: list[str], env: dict[str, str] | None = None) -> None:
    print("+ " + " ".join(argv), flush=True)
    subprocess.run(argv, cwd=ROOT, env=env, check=True)


def image_env() -> dict[str, str]:
    """A desktop with the Terminal, the UI probe and the sound card."""
    env = dict(os.environ)
    env.update(
        {
            "LAZYOS_RESET_OS": "1",
            "LAZYOS_DESKTOP": "1",
            "LAZYOS_XUI_AUTOSTART": "term",
            "LAZYOS_UI_PROBE": "1",
            "LAZYOS_SOUND": "1",
        }
    )
    return env


def build() -> None:
    run([PY, "tools/xui/build.py"])
    run([PY, "tools/nes/make_sample.py", "--check"])
    run(["cargo", "build"], env=image_env())


def session(accel: str) -> None:
    OUT.mkdir(parents=True, exist_ok=True)
    run(
        [
            PY,
            "tools/screenshot/qemu_session.py",
            "--image",
            "target/lazyos.img",
            "--out",
            str(OUT),
            "--script",
            str(SCRIPT),
            "--accel",
            accel,
            "--extra-arg=-audiodev",
            "--extra-arg=f"wav,id=a0,path={WAV}",
            "--extra-arg=-device",
            "--extra-arg=virtio-sound-pci,audiodev=a0",
        ]
    )


def report(failures: list[str], what: str) -> None:
    if failures:
        print(f"NES:{what}:FAIL")
        for failure in failures:
            print(f"  {failure}")
    else:
        print(f"NES:{what}:PASS")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--no-build", action="store_true", help="reuse target/lazyos.img")
    parser.add_argument("--accel", default="auto", choices=["auto", "tcg", "none", "kvm", "whpx"])
    args = parser.parse_args(argv)
    if not args.no_build:
        build()
    session(args.accel)

    log = (OUT / "serial.log").read_text(errors="replace")
    failures = judge.judge_log(log)
    report(failures, "MARKERS")
    if not WAV.exists():
        print("NES:SOUND:FAIL")
        print(f"  {WAV} was not written")
        failures.append("no recording")
    else:
        sound = judge.judge_sound(WAV.read_bytes())
        report(sound, "SOUND")
        failures.extend(sound)
    shots = {
        "playing": OUT / "shot_01_playing.png",
        "moved": OUT / "shot_02_moved.png",
        "paused": OUT / "shot_03_paused.png",
    }
    shots_failed = judge.judge_shots(shots)
    report(shots_failed, "SHOTS")
    failures.extend(shots_failed)
    print(f"nes: {'PASS' if not failures else 'FAIL'} ({OUT})")
    return 0 if not failures else 1


if __name__ == "__main__":
    sys.exit(main())
