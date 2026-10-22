#!/usr/bin/env python3
"""webd's malicious-client attack harness (docs/webd-plan.md P5): a real client of webd that lists
only the base full-view interface tries what a hostile app would — reach the `net` and `browser`
scopes it was not granted, flood input, and exhaust the per-client view cap — and every attempt is
refused or survived.

The client is `os.lazy.webdattack` ("Web Attack Probe", `xui-app/src/bin/webdattack.rs`, logic in
`xui_webd_client::attack`), shipped only with `LAZYOS_WEBDDEMO=1`. The image autostarts it under the
label trace; the client fires its probes against the running webd and prints `WEBDATTACK:*`, then
quits. `attackjudge.py` holds the expectation of each probe (a scope it was not granted is refused by
the kernel, with a `LABEL:DENY` of its label; the cap is the service's `ENOSPC`; webd stays up under
the flood), in the style of `tools/accounts/run.py` and `tools/webkit/attack_run.py`.

    python tools/webd/attack_run.py                  # build, boot, judge
    python tools/webd/attack_run.py --no-build       # the image of the last run
    python tools/webd/attack_run.py --judge-only DIR # judge a saved serial.log

The malicious *page process* (a compromised WebProcess/NetworkProcess) is the other half, and is
`python tools/webkit/attack_run.py --browser`. Exit status is non-zero on any breach. Logs go to
`shots/webd-attack`. A build uses `LAZYOS_RESET_OS=1`.
"""

from __future__ import annotations

import argparse
import os
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
for sub in ("tools/abi", "tools"):
    sys.path.append(str(ROOT / sub))
sys.path.insert(0, str(HERE))
import attackjudge  # noqa: E402
from lazygui import limits  # noqa: E402

PY = sys.executable
IMAGE = ROOT / "target" / "lazyos.img"
RUN_DEMO = ROOT / "tools" / "run_demo.py"
OUT = ROOT / "shots" / "webd-attack"
PREFIX = "WEBDATTACK"


def build_image() -> str | None:
    """Build a desktop image that ships webd and the attack client, autostarted, with the label
    trace; an error message, or None."""
    env = {k: v for k, v in os.environ.items() if not k.startswith("LAZYOS_")}
    env.update(LAZYOS_XUI_AUTOSTART="webdattack", LAZYOS_LABEL_TRACE="1")
    command = [PY, str(RUN_DEMO), "--webd-demo", "--desktop", "--big-app", "--reset-os", "--yes",
               "--autologin", "user", "--build-only"]
    print(f"webdattack: {' '.join(command[2:])} (LAZYOS_XUI_AUTOSTART=webdattack, label trace)", flush=True)
    done = subprocess.run(command, cwd=ROOT, env=env, capture_output=True, text=True)
    if done.returncode != 0:
        return "run_demo.py --webd-demo failed:\n" + (done.stdout + done.stderr)[-4000:]
    return None if IMAGE.is_file() else f"{IMAGE} was not built"


def boot(seconds: int, args) -> str:
    """Boot the image headless for `seconds`; the attack client runs and quits on its own."""
    OUT.mkdir(parents=True, exist_ok=True)
    (OUT / "serial.log").unlink(missing_ok=True)
    command = [PY, str(ROOT / "tools" / "screenshot" / "qemu_shot.py"), "--out", str(OUT),
               "--at", str(seconds), "--accel", args.accel, "--image", str(IMAGE),
               "--memory", args.memory]
    if args.qemu:
        command += ["--qemu", args.qemu]
    subprocess.run(command, cwd=ROOT, capture_output=True, text=True)
    log = OUT / "serial.log"
    return log.read_text(errors="replace") if log.is_file() else ""


def verdict(serial: str) -> int:
    problems, rows = attackjudge.judge(serial)
    for row in rows:
        print(f"  {row}")
    for problem in problems:
        print(f"{PREFIX}:HARNESS:FAIL {problem}")
    if not problems:
        print(f"{PREFIX}:HARNESS:PASS {len(attackjudge.CHECKS)} probes, every one as expected")
    return 0 if not problems else 1


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--no-build", action="store_true", help="use the image of the last run")
    parser.add_argument("--judge-only", metavar="DIR", help="judge DIR/serial.log and stop")
    parser.add_argument("--seconds", type=int, default=60, help="how long to run the guest")
    parser.add_argument("--accel", default="auto", choices=["auto", "none", "tcg", "whpx", "kvm"])
    parser.add_argument("--memory", default=limits.BIG_APP_MEMORY, help="guest RAM (default: big-app 4G)")
    parser.add_argument("--qemu")
    args = parser.parse_args(argv)

    if args.judge_only:
        log = Path(args.judge_only) / "serial.log"
        return verdict(log.read_text(errors="replace"))
    if not args.no_build:
        error = build_image()
        if error:
            print(f"{PREFIX}:HARNESS:FAIL {error}")
            return 1
    elif not IMAGE.is_file():
        print(f"{PREFIX}:HARNESS:FAIL --no-build needs the image of an earlier run ({IMAGE})")
        return 1
    serial = boot(args.seconds, args)
    if not serial:
        print(f"{PREFIX}:HARNESS:FAIL no serial log (QEMU did not run)")
        return 1
    return verdict(serial)


if __name__ == "__main__":
    raise SystemExit(main())
