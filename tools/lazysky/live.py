#!/usr/bin/env python3
"""Drive LazySky against the real service with a throwaway account and judge
that the app survives a long scroll (the "crash and restart" report).

The handle and the app password come from the host environment
(`LAZYSKY_DUMMY_ACCOUNT`, `LAZYSKY_DUMMY_PWD`) and are typed with the session step
`{"type_secret": VAR}`: neither is written to the script, the logs or the
repo, and the serial log and the capture are scanned for the password. Use an
app password of a dummy account, never a main password.

    python tools/lazysky/live.py                # build, boot, sign in, scroll, judge
    python tools/lazysky/live.py --no-build     # reuse target/lazyos.img
    python tools/lazysky/live.py --rounds 40    # scroll longer

The image has no test CA: certificates verify against the Mozilla roots.
Verdict: one `SKY:UP:PASS` (a second one is a restart), no `SKY:PANIC`, no
failure marker, no secret in the log. Output goes to `shots/lazysky_live`.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
sys.path.insert(0, str(HERE))
import judge  # noqa: E402
import run  # noqa: E402
import session  # noqa: E402

ACCOUNT, PASSWORD = "LAZYSKY_DUMMY_ACCOUNT", "LAZYSKY_DUMMY_PWD"
APP = session.APP


def steps(rounds: int, timeout: float) -> list[dict]:
    """Launch (default service), sign in, then scroll `rounds` times."""
    out: list[dict] = [
        {"wait_for": "PKGD:PROVISION:DONE", "timeout": 600},
        {"wait_for": "TERM:UP:PASS", "timeout": 600},
        {"wait_for": "NETD:ADDR 10.0.2.15", "timeout": 300},
    ]
    out += session.typed("echo 'let i = msg::connect(\"os.lazy.init.v1\");' > /tmp/l.rhai", 1.0)
    out += session.typed("echo 'for a in os::args() { print(i.launch(a, \"\", 0)); }' >> /tmp/l.rhai", 2.5)
    out += session.typed(f"clear; rhai /tmp/l.rhai {APP}", 4.0)
    out += [
        {"wait_for": "SKY:UP:PASS", "timeout": timeout},
        {"wait": 2.0},
        {"shot": "01_login"},
        {"type_secret": ACCOUNT, "delay": 0.05},
        {"key": "tab"},
        {"type_secret": PASSWORD, "delay": 0.05},
        {"key": "tab"},
        {"key": "space"},
        {"wait_for": "SKY:FEED:ROWS:", "timeout": timeout},
        {"wait": 5.0},
        {"shot": "02_feed"},
        {"move_to": {"window": "LazySky"}},
    ]
    for n in range(rounds):
        out += [step for _ in range(40) for step in ({"mouse_scroll": -5}, {"wait": 0.1})]
        # "Load more" is a click, not automatic: the wheel (End does not scroll
        # the feed) to the very end, then the row at the bottom; Alt+Left in
        # case the click opened a thread instead.
        out += [step for _ in range(60) for step in ({"mouse_scroll": -10}, {"wait": 0.05})]
        out += [{"wait": 1.0}, {"click_at": {"window": "LazySky", "offset": [0, 292]}}, {"wait": 4.0},
                {"key_down": "alt"}, {"key": "left"}, {"key_up": "alt"}, {"wait": 1.0}]
        if n % 5 == 4:
            out += [{"shot": f"scroll_{n + 1:03d}"}]
    out += [{"wait": 3.0}, {"shot": "end"}, {"wait": 2.0}, {"quit": True}]
    return out


def judge_live(text: str, password: str) -> list[str]:
    """The app stayed up; nothing secret was logged."""
    problems = []
    if "SKY:UP:PASS" not in text:
        problems.append("the app never came up")
    if text.count("SKY:UP:PASS") > 1:
        problems.append(f"the app started {text.count('SKY:UP:PASS')} times: it crashed and restarted")
    launches = re.findall(rf"^INIT:LAUNCH:PASS app={re.escape(APP)}\b.*$", text, re.M)
    if len(launches) > 1:
        problems.append(f"init launched {APP} {len(launches)} times")
    for line in re.findall(r"^SKY:PANIC:[^\n]*$", text, re.M):
        problems.append(f"panic: {line}")
    for line in judge.FAILURES.findall(text):
        problems.append(f"failure marker: {line}")
    for line in re.findall(r"^[^\n]*lazysky[^\n]*(?:crash|exited|killed|oom|fault)[^\n]*$", text, re.M | re.I):
        problems.append(f"init/kernel: {line}")
    if "SKY:FEED:ROWS:" not in text:
        problems.append("the feed never showed rows (sign in failed?)")
    if password and password in text:
        problems.append("the app password appeared in the serial log")
    return problems


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--out", default="shots/lazysky_live")
    parser.add_argument("--no-build", action="store_true")
    parser.add_argument("--rounds", type=int, default=20, help="scroll rounds of 40 notches (default 20)")
    parser.add_argument("--accel", default="auto", choices=["auto", "none", "tcg", "whpx", "kvm"])
    parser.add_argument("--memory", default="1G")
    parser.add_argument("--step-timeout", type=float, default=300.0)
    parser.add_argument("--qemu")
    args = parser.parse_args(argv)
    account, password = os.environ.get(ACCOUNT, ""), os.environ.get(PASSWORD, "")
    if not account or not password:
        print(f"LAZYSKY:LIVE:FAIL set {ACCOUNT} and {PASSWORD} (an app password) in this environment")
        return 1
    out = Path(args.out) if Path(args.out).is_absolute() else ROOT / args.out
    out.mkdir(parents=True, exist_ok=True)
    for stale in ["serial.log", "net.pcap", "summary.json", *(p.name for p in out.glob("shot_*.png"))]:
        (out / stale).unlink(missing_ok=True)
    if not args.no_build:
        error = run.build({}, False)
        if error:
            print(f"LAZYSKY:LIVE:FAIL {error}")
            return 1
    script = out / "session.json"
    script.write_text(json.dumps(steps(args.rounds, args.step_timeout), indent=1) + "\n", encoding="utf-8")
    command = [sys.executable, str(ROOT / "tools" / "screenshot" / "qemu_session.py"),
               "--image", str(run.IMAGE), "--out", str(out), "--script", str(script),
               "--accel", args.accel, "--net", "--net-forward", "none",
               "--net-pcap", str(out / "net.pcap"), "--memory", args.memory,
               "--fail-on", "PANIC", "--fail-on", "EXCEPTION"]
    if args.qemu:
        command += ["--qemu", args.qemu]
    with open(out / "session.log", "w", encoding="utf-8") as console:
        code = subprocess.call(command, cwd=ROOT, env=dict(os.environ), stdout=console,
                               stderr=subprocess.STDOUT)
    log = out / "serial.log"
    text = log.read_text(errors="replace") if log.is_file() else ""
    problems = judge_live(text, password)
    if code != 0:
        problems.append("the session did not finish (see session.log, summary.json)")
    for problem in problems[:16]:
        print(f"LAZYSKY:LIVE:FAIL {problem}")
    perf = judge.perf_lines(text)
    worst = max((p["tick_max"] for p in perf), default=0.0)
    print(f"LAZYSKY:LIVE:{'FAIL' if problems else 'PASS'} perf_reports={len(perf)} worst_tick={worst} ms")
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
