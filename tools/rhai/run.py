#!/usr/bin/env python3
"""Build `rhai`, put it in an image, boot it and check it: one command.

The same steps CI's `.github/workflows/rhai.yml` runs, in order, with every
missing prerequisite reported as an error instead of skipped:

1. `tools/rhai/build.py`: the static-musl `rhai` command (`target/rhai/rhai.elf`);
2. BusyBox, the shell that runs it (`tools/abi/busybox.py`; a git worktree
   reuses the main checkout's cached build, so it needs no Docker);
3. `cargo build` with `LAZYOS_CLI=1` (console boot, `/system/bin/rhai` embedded);
4. a headless QEMU session typing `tools/screenshot/examples/rhai_demo.json`,
   which prints `RHAI:<check>:PASS|FAIL` markers on serial;
5. with `--desktop`, the xui apps and two desktop Terminal sessions: the REPL
   (`rhai_desktop.json`) and the `msg` module against the real services
   (`rhai_msg.json`: list services, call confd, a set/get round trip, the
   topics broker, a refused call, a missing service) and the event loop
   (`rhai_msg_loop.json`: a topic round trip, a confd change event, and a
   service written in Rhai answering another script);
6. with `--lazyrad`, LazyRAD and the Messenger sample (`lazyrad-os/samples/
   messenger`) on the desktop: `lazyrad_msg.json` starts the form, and
   `poke.rhai` changes the confd key it watches and calls the service it
   serves (`docs/lazyrad-messenger-plan.md`).

    python tools/rhai/run.py                 # console checks
    python tools/rhai/run.py --desktop       # plus the desktop Terminal and msg
    python tools/rhai/run.py --msg-only      # just the msg session (desktop image)
    python tools/rhai/run.py --lazyrad       # just the LazyRAD Messenger session
    python tools/rhai/run.py --no-build      # reuse target/lazyos.img
    python tools/rhai/run.py --accel none    # force TCG

Exit status is non-zero on any failure; logs and screenshots go to `shots/rhai*`.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent
sys.path.insert(0, str(ROOT / "tools" / "abi"))
import busybox  # noqa: E402

PY = sys.executable
SESSION = ROOT / "tools" / "screenshot" / "qemu_session.py"
EXAMPLES = ROOT / "tools" / "screenshot" / "examples"
IMAGE = ROOT / "target" / "lazyos.img"
#: What the LazyRAD Messenger session must show on serial (lazyrad_msg.json):
#: `msg`/`sys` installed, the form up, the form's service answered poke.rhai,
#: and a Messenger handler (the confd change, the served call) ran in the form.
LAZYRAD_MARKERS = (
    "LRPLAY:MSG:PASS",
    "LRPLAY:UP:PASS",
    "TERM:OUT:RHAI:lrpoke:poked:hihi",
    "LRPLAY:MSGEVENT:PASS",
)
#: The LazyOS-only LazyRAD samples the image embeds under /system/share/lazyrad/.
LAZYRAD_SAMPLES = "lazyrad-os/samples/messenger"
#: The console session reports this many distinct checks (see rhai_demo.json).
MIN_CONSOLE_PASSES = 34
#: Lines the desktop Terminal must echo to serial (rhai_desktop.json).
DESKTOP_OUTPUT = ("TERM:OUT:42", "TERM:OUT:HI", "TERM:OUT:123", "TERM:OUT:144", "TERM:OUT:RHAI:desktop:42")
#: rhai_msg.json builds /tmp/m from short lines (the Terminal logs one
#: `TERM:OUT` line per command, so commands must not wrap) and the script
#: prints how many of its six checks passed, then their names.
MSG_MARKER = "TERM:OUT:RHAI:msg:6"
#: rhai_msg_loop.json: topic round trip, a confd change event, and a service
#: written in Rhai (run in the background) called from another script.
MSG_LOOP_MARKER = "TERM:OUT:RHAI:msg2:4"


def fail(message: str) -> int:
    print(f"RHAI:HARNESS:FAIL: {message}", file=sys.stderr)
    return 1


def build_tool(script: str, key: str) -> Path | None:
    """Run a `tools/*/build.py` that prints a JSON map; the path of `key`."""
    result = subprocess.run([PY, str(ROOT / script)], cwd=ROOT, capture_output=True, text=True)
    sys.stderr.write(result.stderr)
    if result.returncode != 0:
        return None
    start = result.stdout.find("{")
    built = json.loads(result.stdout[start:]) if start >= 0 else {}
    return Path(built[key]) if key in built else None


def find_busybox() -> Path | None:
    explicit = os.environ.get("LAZYOS_BUSYBOX")
    if explicit and Path(explicit).is_file():
        return Path(explicit)
    return busybox.ensure_busybox()


def cargo_build(env_extra: dict[str, str]) -> bool:
    env = dict(os.environ, **env_extra)
    label = " ".join(f"{k}={v}" for k, v in env_extra.items() if k != "LAZYOS_BUSYBOX")
    print(f"rhai: {label} cargo build", flush=True)
    result = subprocess.run(["cargo", "build"], cwd=ROOT, env=env, capture_output=True, text=True)
    if result.returncode != 0:
        sys.stderr.write(result.stderr[-4000:])
        return False
    return IMAGE.is_file()


def run_session(script: str, out: str, args: argparse.Namespace, fail_on: list[str]) -> str:
    """Boot the image and drive one session; the serial log text."""
    command = [
        PY, str(SESSION), "--image", str(IMAGE), "--out", out, "--timeout", str(args.timeout),
        "--script", str(EXAMPLES / script), "--accel", args.accel,
    ]
    for pattern in fail_on:
        command += ["--fail-on", pattern]
    if args.qemu:
        command += ["--qemu", args.qemu]
    print(f"rhai: session {script}", flush=True)
    code = subprocess.call(command, cwd=ROOT)
    log = ROOT / out / "serial.log"
    text = log.read_text(errors="replace") if log.is_file() else ""
    if code != 0:
        print(f"rhai: {script} exited with {code}", file=sys.stderr)
    return text


def check_console(text: str) -> list[str]:
    markers = sorted(set(re.findall(r"^RHAI:[a-z]+:(?:PASS|FAIL)\S*", text, re.M)))
    for marker in markers:
        print(f"  {marker}")
    problems = [m for m in markers if ":FAIL" in m]
    passes = sum(1 for m in markers if m.endswith(":PASS"))
    if passes < MIN_CONSOLE_PASSES:
        problems.append(f"only {passes} of {MIN_CONSOLE_PASSES} console checks passed")
    if "RHAI:repl:PASS" not in markers:
        problems.append("the REPL check did not pass")
    return problems


def check_desktop(text: str) -> list[str]:
    return [f"missing {line}" for line in DESKTOP_OUTPUT if line not in text]


def check_marker(text: str, marker: str, out: str) -> list[str]:
    prefix = marker.rsplit(":", 1)[0] + ":"
    found = re.findall(re.escape(prefix) + r"\d+", text)
    print(f"  {found[-1] if found else 'no ' + prefix + ' marker'} (want {marker})")
    return [] if marker in text else [f"{marker} missing (see {out})"]


def lazyrad_session(env: dict[str, str], args: argparse.Namespace) -> list[str]:
    """Build LazyRAD into a desktop image and run the Messenger sample."""
    if not args.no_build:
        if build_tool("tools/lazyrad/build.py", "lrplay") is None:
            return ["tools/lazyrad/build.py produced no lrplay"]
        if build_tool("tools/xui/build.py", "xui-term") is None:
            return ["tools/xui/build.py produced no xui-term"]
        image_env = {**env, "LAZYOS_DESKTOP": "1", "LAZYOS_XUI_AUTOSTART": "term",
                     "LAZYOS_LAZYRAD": "1", "LAZYRAD_SAMPLES": LAZYRAD_SAMPLES}
        if not cargo_build(image_env):
            return ["cargo build (LAZYOS_DESKTOP=1 LAZYOS_LAZYRAD=1) failed"]
    out = "shots/lazyrad_msg"
    text = run_session("lazyrad_msg.json", out, args,
                       ["TERM:(BIND|RUN|SPAWN|PANIC)", "LRPLAY:[A-Z]+:FAIL", "RHAI:lrpoke:no-service"])
    problems = []
    for marker in LAZYRAD_MARKERS:
        print(f"  {'ok ' if marker in text else 'missing'} {marker}")
        if marker not in text:
            problems.append(f"{marker} missing (see {out})")
    return problems


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--no-build", action="store_true", help="reuse target/lazyos.img (console only)")
    parser.add_argument("--desktop", action="store_true", help="also run the desktop Terminal sessions")
    parser.add_argument("--msg-only", action="store_true",
                        help="build the desktop image and run only the msg sessions")
    parser.add_argument("--lazyrad", action="store_true",
                        help="build LazyRAD and run only the LazyRAD Messenger session")
    parser.add_argument("--accel", default="auto", choices=["auto", "none", "tcg", "whpx", "kvm"])
    parser.add_argument("--qemu", help="path to qemu-system-x86_64")
    parser.add_argument("--timeout", type=float, default=300.0, help="seconds per session")
    args = parser.parse_args()

    if args.msg_only:
        args.desktop = True
    only_desktop = args.msg_only or args.lazyrad
    env: dict[str, str] = {}
    if not args.no_build:
        if build_tool("tools/rhai/build.py", "rhai") is None:
            return fail("tools/rhai/build.py produced no rhai (see the messages above)")
        shell = find_busybox()
        if shell is None:
            return fail("no BusyBox: run `python tools/abi/busybox.py` (Linux with musl-gcc, "
                        "or Docker), or set LAZYOS_BUSYBOX to a static busybox")
        env["LAZYOS_BUSYBOX"] = str(shell)
        if not only_desktop and not cargo_build({**env, "LAZYOS_CLI": "1"}):
            return fail("cargo build (LAZYOS_CLI=1) failed")

    problems: list[str] = []
    if args.lazyrad:
        problems += lazyrad_session(env, args)
    elif not args.msg_only:
        problems += check_console(run_session(
            "rhai_demo.json", "shots/rhai", args, ["RHAI:[a-z]+:FAIL", "user: task [0-9]+ killed by"]))

    if args.desktop and not problems:
        if not args.no_build:
            if build_tool("tools/xui/build.py", "xui-term") is None:
                return fail("tools/xui/build.py produced no xui-term")
            if not cargo_build({**env, "LAZYOS_DESKTOP": "1", "LAZYOS_XUI_AUTOSTART": "term"}):
                return fail("cargo build (LAZYOS_DESKTOP=1) failed")
        term_failures = ["TERM:(BIND|RUN|SPAWN|PANIC)"]
        if not args.msg_only:
            problems += check_desktop(run_session(
                "rhai_desktop.json", "shots/rhai_desktop", args, term_failures))
        problems += check_marker(run_session(
            "rhai_msg.json", "shots/rhai_msg", args, term_failures + ["TERM:OUT:RHAI:msg:[0-5]$"]),
            MSG_MARKER, "shots/rhai_msg")
        problems += check_marker(run_session(
            "rhai_msg_loop.json", "shots/rhai_msg_loop", args, term_failures + ["TERM:OUT:RHAI:msg2:[0-3]$"]),
            MSG_LOOP_MARKER, "shots/rhai_msg_loop")

    if problems:
        for problem in problems:
            print(f"rhai: {problem}", file=sys.stderr)
        return fail(f"{len(problems)} problem(s)")
    print("RHAI:HARNESS:PASS")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
