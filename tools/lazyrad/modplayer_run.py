#!/usr/bin/env python3
"""Build the LazyRAD MOD player, play it on LazyOS, record it and judge it.

Two sessions on a desktop image with a virtio-sound card recorded to WAV:

* ``dev``: the sample run from the Terminal (the core package's player,
  ``/apps/os.lazy.lazyrad/*/bin/lrplay.elf --client /system/share/lazyrad/modplayer``): play, mute a channel, pause, resume, play to the end
  (``tools/screenshot/examples/lazyrad_modplayer.json``);
* ``installed``: the package copied from ``/system/share/samples`` to the
  home and installed with ``pkgctl install``, started from the Start menu
  as the installed app ``org.lazy.modplayer`` under its manifest's rules,
  played to the end (``lazyrad_modplayer_installed.json``).

Each session boots its own copy of the image, so the run repeats without
rebuilding. The verdict is the serial markers (``LRPLAY:MODPLAY:PASS ...
audio=1``, ``LRPLAY:MODEND:PASS``, the app label for the installed run) and the
recording, which must be the whole song in order (``modjudge.py``, against the
same module rendered on the host by ``libs/modplay``).

Usage::

    python tools/lazyrad/modplayer_run.py                 # build, both sessions, judge
    python tools/lazyrad/modplayer_run.py --no-build      # reuse target/lazyos.img
    python tools/lazyrad/modplayer_run.py --session dev   # one session only
"""

from __future__ import annotations

import argparse
import os
import shutil
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
sys.path.insert(0, str(HERE))

import modjudge  # noqa: E402

PY = sys.executable
IMAGE = ROOT / "target" / "lazyos.img"
EXAMPLES = ROOT / "tools" / "screenshot" / "examples"
SAMPLES = ["lazyrad-os/samples/messenger", "lazyrad-os/samples/modplayer"]

#: name -> (session script, markers the serial log must show)
SESSIONS = {
    "dev": ("lazyrad_modplayer.json",
            ["LRPLAY:MODPLAY:PASS:title=\"LazyOS Groove\" audio=1", "LRPLAY:MODEND:PASS"]),
    "installed": ("lazyrad_modplayer_installed.json",
                  ["PKGD:INSTALL:PASS org.lazy.modplayer",
                   "PKGD:LAUNCH:LABEL app:org.lazy.modplayer",
                   "LRPLAY:MODPLAY:PASS:title=\"LazyOS Groove\" audio=1", "LRPLAY:MODEND:PASS"]),
}


def run(argv: list[str], env: dict[str, str] | None = None) -> None:
    print("+ " + " ".join(argv), flush=True)
    subprocess.run(argv, cwd=ROOT, env=env, check=True)


def image_env() -> dict[str, str]:
    """The image the sessions need: desktop, LazyRAD and its samples, the
    package in /system/share/samples, sound, and a fresh OS volume."""
    env = dict(os.environ)
    env.update({
        "LAZYOS_RESET_OS": "1",
        "LAZYOS_DESKTOP": "1",
        "LAZYOS_LAZYRAD": "1",
        "LAZYOS_MODPLAYER": "1",
        "LAZYOS_SOUND": "1",
        "LAZYRAD_SAMPLES": os.pathsep.join(SAMPLES),
    })
    return env


def build() -> None:
    # The desktop's apps and core packages (incremental when unchanged).
    run([PY, "tools/xui/build.py"])
    run([PY, "tools/lazyrad/build.py"])
    run([PY, "tools/lazyrad/package.py", "--no-build", "--require"])
    run(["cargo", "build"], env=image_env())


def reference(out: Path) -> modjudge.Audio:
    """The built-in song rendered on the host at the deck's rate."""
    mod, wav = out / "demo.mod", out / "reference.wav"
    run([PY, "tools/lazyrad/gen_demo_song.py", "--check", "--mod", str(mod)])
    run(["cargo", "run", "--quiet", "--release", "--manifest-path", "libs/modplay/Cargo.toml",
         "--example", "render_wav", "--", str(mod), str(wav), "22050"])
    return modjudge.read_wav(wav.read_bytes())


def session(name: str, out: Path, accel: str) -> list[str]:
    """Run one session on a copy of the image; returns the problems found."""
    script, markers = SESSIONS[name]
    folder = out / name
    shutil.rmtree(folder, ignore_errors=True)
    folder.mkdir(parents=True)
    image = folder / "lazyos.img"
    shutil.copyfile(IMAGE, image)
    wav = folder / "out.wav"
    argv = [PY, "tools/screenshot/qemu_session.py", "--image", str(image), "--out", str(folder),
            "--script", str(EXAMPLES / script), "--memory", "1G", "--accel", accel,
            "--fail-on", "LRPLAY:[A-Z]+:FAIL",
            "--extra-arg=-audiodev", f"--extra-arg=wav,id=a0,path={wav}",
            "--extra-arg=-device", "--extra-arg=virtio-sound-pci,audiodev=a0"]
    print("+ " + " ".join(argv), flush=True)
    finished = subprocess.run(argv, cwd=ROOT).returncode == 0
    image.unlink(missing_ok=True)
    problems = [] if finished else [f"{name}: the session failed (see {folder})"]
    serial = (folder / "serial.log").read_text(errors="replace") if (folder / "serial.log").is_file() else ""
    problems += [f"{name}: no `{m}` on serial" for m in markers if m not in serial]
    return problems


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--no-build", action="store_true", help="reuse target/lazyos.img")
    parser.add_argument("--session", choices=[*SESSIONS, "all"], default="all")
    parser.add_argument("--accel", default="auto", help="QEMU accelerator (default: auto)")
    parser.add_argument("--out", type=Path, default=ROOT / "shots" / "modplayer-run")
    args = parser.parse_args(argv)

    if not args.no_build:
        build()
    args.out.mkdir(parents=True, exist_ok=True)
    song = reference(args.out)
    names = list(SESSIONS) if args.session == "all" else [args.session]
    problems: list[str] = []
    for name in names:
        found = session(name, args.out, args.accel)
        wav = args.out / name / "out.wav"
        if wav.is_file():
            verdict = modjudge.judge(modjudge.read_wav(wav.read_bytes()), song)
            print(f"{name}: " + "\n  ".join(verdict.lines))
            if not verdict.ok:
                found.append(f"{name}: the recording is not the song")
        else:
            found.append(f"{name}: nothing was recorded")
        print(f"MODPLAYER:{name.upper()}:{'PASS' if not found else 'FAIL'}", flush=True)
        problems += found
    for problem in problems:
        print("  " + problem, file=sys.stderr)
    return 1 if problems else 0


if __name__ == "__main__":
    raise SystemExit(main())
