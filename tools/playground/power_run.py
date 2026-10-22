#!/usr/bin/env python3
"""Guest check: a quick double or triple click on the start menu's session rows never acts.

`tools/screenshot/examples/power_dblclick.json` clicks "Log out...", "Restart..." and "Shut
down..." three times in quick succession each. The first press swaps the row for its confirmation;
the second becomes a lone `MouseDoubleClick` the model ignores as a repeat (`Menu::choose`); the
third is a plain press again, because the backend's click tracker consumes a pair, and lands on the
confirmation (or "Cancel") now under the pointer: only the model's quiet period after the swap
(`CONFIRM_QUIET_TICKS`) stops it. The session runs with `--fail-on SHELL:POWER:REQUEST` (a reboot
or a shut down would end it at once) and this judge then requires

* one `SHELL:POWER:CONFIRM` per row (the confirmation was shown each time) and no
  `SHELL:POWER:REQUEST`, `SHELL:LOGOUT:REQUEST` or their `:FAIL` forms (nothing was asked of
  `init` or `logind`);
* the desktop still up at the end (`SHELL:MENU:CLOSE` after each Cancel) and its screenshots
  present and not blank.

    python tools/playground/power_run.py                 # boot target/lazyos.img, judge
    python tools/playground/power_run.py --out-dir DIR   # judge an earlier run's DIR (no boot)

The image is the one `python tools/playground/run.py` builds (any desktop image with the UI probe
works). Exit status is non-zero on any failure; `test_judge.py` tests the judge.
"""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
sys.path.insert(0, str(HERE))
import judge  # noqa: E402

EXAMPLE = ROOT / "tools" / "screenshot" / "examples" / "power_dblclick.json"
SESSION = ROOT / "tools" / "screenshot" / "qemu_session.py"
ROWS = 3
SHOTS = ["01_logout_triple_click_still_asking", "02_restart_triple_click_still_asking",
         "03_shutdown_triple_click_still_asking", "04_menu_closed_desktop_still_up"]
#: Anything that asks `init` or `logind` to act (a refusal is as bad: the click got through).
ACTED = re.compile(r"^SHELL:(POWER:REQUEST|LOGOUT:REQUEST|LOGOUT:FAIL)", re.M)


def judge_power(text: str) -> list[str]:
    """Problems with the serial log of the power session."""
    problems = judge.judge_integrity(text)
    for match in ACTED.finditer(text):
        problems.append(f"a quick click reached the action: {match.group(0)}")
    confirms = text.count("SHELL:POWER:CONFIRM")
    if confirms != ROWS:
        problems.append(f"expected {ROWS} SHELL:POWER:CONFIRM lines, saw {confirms}")
    closes = text.count("SHELL:MENU:CLOSE")
    if closes < ROWS:
        problems.append(f"expected the menu closed by Cancel {ROWS} times, saw {closes}")
    return problems


def boot(image: Path, out: Path, args: argparse.Namespace) -> int:
    command = [sys.executable, str(SESSION), "--image", str(image), "--out", str(out),
               "--script", str(EXAMPLE), "--timeout", str(args.timeout), "--accel", args.accel,
               "--fail-on", "SHELL:POWER:REQUEST", "--fail-on", "SHELL:LOGOUT:REQUEST",
               "--fail-on", "LazyOS PANIC"]
    if args.qemu:
        command += ["--qemu", args.qemu]
    return subprocess.call(command, cwd=ROOT)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--image", default=str(ROOT / "target" / "lazyos.img"))
    parser.add_argument("--out-dir", help="judge this earlier run's directory instead of booting")
    parser.add_argument("--accel", default="auto", choices=["auto", "none", "tcg", "whpx", "kvm"])
    parser.add_argument("--timeout", type=int, default=600)
    parser.add_argument("--qemu")
    args = parser.parse_args(argv)
    out = Path(args.out_dir) if args.out_dir else ROOT / "shots" / "playground" / "power"
    if not args.out_dir:
        code = boot(Path(args.image), out, args)
        if code != 0:
            print(f"PLAYGROUND:POWER:FAIL: the session exited with {code}", file=sys.stderr)
            return 1
    log = out / "serial.log"
    if not log.is_file():
        print(f"PLAYGROUND:POWER:FAIL: no serial log in {out}", file=sys.stderr)
        return 1
    problems = judge_power(log.read_text(errors="replace")) + judge.judge_shots(out, SHOTS)
    for problem in problems:
        print(f"PLAYGROUND:POWER:FAIL: {problem}", file=sys.stderr)
    if not problems:
        print("PLAYGROUND:POWER:PASS")
    return 1 if problems else 0


if __name__ == "__main__":
    raise SystemExit(main())
