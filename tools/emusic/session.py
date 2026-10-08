#!/usr/bin/env python3
"""Generate the emusic sessions: ``tools/screenshot/examples/emusic.json``
(the app, P3) and ``emusic_sound.json`` (the sound check ``tools/emusic/run.py``
records, P4).

The session installs ``/system/share/samples/emusic.lzp`` from the Terminal,
copies the package's sample track to ``~/Music`` and opens it through
``mimed`` (``audio/mpeg`` -> ``org.lazy.emusic``), then waits for emusic's
markers (``EMUSIC:UP``, ``EMUSIC:PLAY``, ``EMUSIC:POS``, ``EMUSIC:END``) and
takes a screenshot as the position moves. The sound session runs
``emusic.elf --sound-check`` on the same track from the Terminal instead. Commands are typed in short chunks,
each checked against the Terminal's ``TERM:CMD`` echo, as in ``doom.json``.

    python tools/emusic/session.py --write    # regenerate both
    python tools/emusic/session.py --check    # fail when one is stale
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
EXAMPLES = ROOT / "tools" / "screenshot" / "examples"
CHUNK = 6
#: The sample track, where the session copies it.
TRACK = "/home/user/Music/tones.mp3"


def command(text: str, done: str | None = None, timeout: int = 120) -> list[dict]:
    """Type `text` in chunks, press Enter until the Terminal echoes it, then
    wait for `done` on the serial console."""
    steps = [{"at": 0.4, "type": text[i:i + CHUNK]} for i in range(0, len(text), CHUNK)]
    steps.append({"at": 0.8, "key": "enter", "until": f"TERM:CMD:{text}",
                  "timeout": 30, "retries": 2})
    if done:
        steps.append({"wait_for": done, "timeout": timeout})
    return steps


def install() -> list[dict]:
    """Boot, install the package from the Terminal and put the sample track
    in `~/Music`."""
    steps: list[dict] = [
        {"wait_for": "PKGD:PROVISION:DONE", "timeout": 600},
        {"wait_for": "TERM:UP:PASS", "timeout": 300},
        {"wait_for": "PKGD:UP:PASS", "timeout": 120},
        {"wait_for": "PKGD:RECONCILE:PASS", "timeout": 120},
        {"at": 2.0, "click_at": {"window": "Terminal"}, "note": "focus the Terminal"},
    ]
    steps += command("PS1='# '")
    steps += command("cp /system/share/samples/emusic.lzp ~/ && echo staged",
                     "TERM:OUT:staged")
    steps += command("pkgctl install ~/emusic.lzp", "PKGD:INSTALL:PASS org.lazy.emusic", 900)
    steps.append({"wait_for": "TERM:OUT:PKGCTL:INSTALL:PASS", "timeout": 60})
    steps += command("cd /apps/org.lazy.emusic/*/resources")
    # Not `mkdir -p`: BusyBox's walks up through `/home`, which the session
    # user may not create in, and stops there.
    steps += command("mkdir ~/Music; cp tones.mp3 ~/Music/ && cd && echo copied",
                     "TERM:OUT:copied")
    steps.append({"at": 0.5, "shot": "01_installed"})
    return steps


def app_session() -> list[dict]:
    """Open the track through mimed and watch emusic play it."""
    steps = install()
    steps += command(f"rhai -e 'sys::mimed::open(\"{TRACK}\",\"open\")'",
                     "EMUSIC:UP:PASS", 300)
    steps += [
        {"wait_for": "EMUSIC:PLAY:tones.mp3", "timeout": 120},
        {"wait_for": "EMUSIC:POS:1", "timeout": 60},
        {"at": 0.3, "shot": "02_playing"},
        {"wait_for": "EMUSIC:POS:3", "timeout": 60},
        {"at": 0.1, "shot": "03_position_moved"},
        {"wait_for": "EMUSIC:END:tones.mp3", "timeout": 60},
        {"at": 1.0, "shot": "04_ended"},
        {"at": 1.5, "quit": True},
    ]
    return steps


def sound_session() -> list[dict]:
    """Run the headless sound check on the track, for the recording."""
    steps = install()
    steps += command("cd /apps/org.lazy.emusic/*/bin")
    # The Terminal reports a command's first line of output: keep the last;
    # `sync` puts the detailed log (~/.apps/org.lazy.emusic/sound-check.log) on disk.
    steps += command("./emusic.elf --sound-check ~/Music/tones.mp3|tail -1;sync",
                     "TERM:OUT:EMUSIC:CHECK:DONE", 180)
    steps += [{"at": 0.5, "shot": "02_checked"}, {"at": 1.0, "quit": True}]
    return steps


SESSIONS = {"emusic.json": app_session, "emusic_sound.json": sound_session}


def render(steps: list[dict]) -> str:
    lines = ",\n".join("  " + json.dumps(step) for step in steps)
    return f"[\n{lines}\n]\n"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    group = parser.add_mutually_exclusive_group(required=True)
    group.add_argument("--write", action="store_true")
    group.add_argument("--check", action="store_true")
    args = parser.parse_args()
    stale = []
    for name, make in SESSIONS.items():
        path = EXAMPLES / name
        text = render(make())
        if args.write:
            path.write_text(text, encoding="utf-8", newline="\n")
            print(path)
        elif not path.is_file() or path.read_text(encoding="utf-8") != text:
            stale.append(name)
    if stale:
        print(f"stale: {', '.join(stale)}: run python tools/emusic/session.py --write",
              file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
