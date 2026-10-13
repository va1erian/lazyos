#!/usr/bin/env python3
"""Network time, end to end (docs/ntp-plan.md section 5).

Builds one services console image (`LAZYOS_SERVICES=1`: `init` supervises
`timed` and the rest; the console login is typed in, as `user`/`lazy`) whose
`timed` is pointed at a host server
(`LAZYOS_TIMED_ARGS=ntp.server=10.0.2.2:PORT`, which also switches the client
on), then boots it once per scenario against `fake_ntpd.py`, which answers
well, kisses, lies, or stays silent, with an RTC that is right or years off.
Each scenario is judged from the guest's serial log (`TIMED:NTP:*`, the time
`timectl now` prints) and from what the host server received (shape, nonces,
spacing of the requests).

    python tools/ntp/run.py                  # build, run every scenario, judge
    python tools/ntp/run.py --only dead_rtc  # one scenario (repeatable)
    python tools/ntp/run.py --no-build       # reuse target/lazyos.img (built by this script)
    python tools/ntp/run.py --list
    python tools/ntp/test_judge.py           # the judge fails when it should

Needs BusyBox (`tools/abi/busybox.py`). Output: `shots/ntp/<scenario>/`.
Exit status is non-zero on any failure.
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(ROOT / "tools" / "abi"))
import busybox  # noqa: E402
import ntp_judge  # noqa: E402
import scenarios  # noqa: E402
from fake_ntpd import FakeNtpd  # noqa: E402

PY = sys.executable
IMAGE = ROOT / "target" / "lazyos.img"
#: The host port the guest asks (10.0.2.2 is QEMU's alias for the host).
PORT = 47123


def build() -> str | None:
    """Build the image; an error message, or None."""
    shell = busybox.ensure_busybox()
    if shell is None:
        return "no BusyBox for the console shell (see tools/abi/busybox.py)"
    env = dict(os.environ, LAZYOS_SERVICES="1", LAZYOS_NETD="1", LAZYOS_NETD_ARGS="demo=0",
               LAZYOS_TIMED_ARGS=f"ntp.server=10.0.2.2:{PORT}", LAZYOS_RESET_OS="1",
               LAZYOS_BUSYBOX=str(shell))
    env.pop("LAZYOS_DESKTOP", None)
    print("ntp: LAZYOS_SERVICES=1 LAZYOS_NETD=1 LAZYOS_TIMED_ARGS=ntp.server=... cargo build", flush=True)
    result = subprocess.run(["cargo", "build"], cwd=ROOT, env=env, capture_output=True, text=True)
    if result.returncode != 0:
        return "cargo build failed:\n" + result.stderr[-4000:]
    return None if IMAGE.is_file() else f"{IMAGE} was not built"


def boot(scenario: scenarios.Scenario, out: Path, args) -> tuple[bool, str, tuple[float, float], list]:
    """One boot against one server; (session ok, serial log, host window, requests)."""
    out.mkdir(parents=True, exist_ok=True)
    for stale in ("serial.log", "summary.json"):
        (out / stale).unlink(missing_ok=True)
    steps = [*scenario.steps, {"quit": True}]
    script = out / "session.json"
    script.write_text(json.dumps(steps, indent=1))
    command = [PY, str(ROOT / "tools" / "screenshot" / "qemu_session.py"), "--image", str(IMAGE),
               "--out", str(out), "--script", str(script), "--accel", args.accel,
               "--net", "--net-forward", "none", "--fail-on", "PANIC", "--fail-on", "EXCEPTION"]
    if scenario.rtc_base:
        command += ["--extra-arg=-rtc", f"--extra-arg=base={scenario.rtc_base}"]
    if args.qemu:
        command += ["--qemu", args.qemu]
    if args.memory:
        command += ["--memory", args.memory]
    server = FakeNtpd(PORT, scenario.script)
    started = time.time()
    try:
        code = subprocess.call(command, cwd=ROOT, stdout=subprocess.DEVNULL)
    finally:
        requests = server.snapshot()
        server.close()
    window = (started, time.time())
    log = out / "serial.log"
    return code == 0, log.read_text(errors="replace") if log.is_file() else "", window, requests


def judge_one(scenario: scenarios.Scenario, ok: bool, serial: str, window, requests) -> list[str]:
    problems = [] if ok else ["the session did not finish (see summary.json)"]
    ctx = ntp_judge.Context(serial, requests, window)
    return problems + ntp_judge.timed_started_once(serial) + scenario.check(ctx)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--out", default="shots/ntp")
    parser.add_argument("--no-build", action="store_true", help="reuse the image (built by this script)")
    parser.add_argument("--accel", default="auto", choices=["auto", "none", "tcg", "whpx", "kvm"])
    parser.add_argument("--qemu")
    parser.add_argument("--memory")
    parser.add_argument("--only", action="append", default=[], metavar="NAME")
    parser.add_argument("--list", action="store_true")
    args = parser.parse_args(argv)
    catalogue = scenarios.catalogue()
    if args.list:
        for s in catalogue:
            print(f"{s.name:14} {s.what}")
        return 0
    chosen = [s for s in catalogue if not args.only or s.name in args.only]
    unknown = set(args.only) - {s.name for s in catalogue}
    if unknown:
        print(f"NTP:HARNESS:FAIL unknown scenario {sorted(unknown)}")
        return 1
    out = Path(args.out) if Path(args.out).is_absolute() else ROOT / args.out
    if not args.no_build:
        error = build()
        if error:
            print(f"NTP:HARNESS:FAIL {error}")
            return 1
    elif not IMAGE.is_file():
        print(f"NTP:HARNESS:FAIL {IMAGE} not found")
        return 1
    failed = False
    for scenario in chosen:
        try:
            ok, serial, window, requests = boot(scenario, out / scenario.name, args)
        except OSError as exc:
            print(f"NTP:HARNESS:FAIL cannot start the host server on UDP {PORT}: {exc}")
            return 1
        problems = judge_one(scenario, ok, serial, window, requests)
        for problem in problems[:10]:
            print(f"NTP:{scenario.name}:FAIL {problem}")
        if not problems:
            print(f"NTP:{scenario.name}:PASS {scenario.what} ({len(requests)} requests)")
        failed |= bool(problems)
    print("NTP:HARNESS:" + ("FAIL" if failed else "PASS"))
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
