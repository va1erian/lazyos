#!/usr/bin/env python3
"""Boot LazyOS with the WebKit port and judge what ``webkit-smoke`` prints (W4).

Needs ``python tools/webkit/port.py`` first (``target/webkit/bin/{WebProcess,
NetworkProcess,webkit-smoke}``). Then:

1. builds a console image with ``LAZYOS_WEBKIT=1`` (the three programs in
   ``/system/bin`, ``fonts.conf`` in ``/system/etc/fonts``) and networking;
2. boots it headless with 4 GiB (three static WebKit processes of tens of MB
   each, loaded whole into RAM), types ``webkit-smoke`` at the shell and waits
   for ``WEBKIT:W4:PASS`` or a ``WEBKIT:W4:FAIL`` line;
3. judges the serial log (``portjudge.py``) and reports the stage reached, so a
   run that dies on the way says where.

Usage::

    python tools/webkit/port_run.py [--qemu PATH] [--accel auto]
    python tools/webkit/port_run.py --no-build        # boot the last image again
    python tools/webkit/port_run.py --judge-only LOG  # judge a saved serial log
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(ROOT / "tools" / "abi"))
import portjudge  # noqa: E402

BIN = ROOT / "target" / "webkit" / "bin"
IMAGE = ROOT / "target" / "webkit" / "lazyos-webkit-port.img"
HOST_RUN = ROOT / "target" / "webkit" / "smoke.host.txt"
PY = sys.executable
PROMPT = "/ #"
#: The program's last line.
FINISHED = r"WEBKIT:W4:(PASS|FAIL:[^\r\n]*)\r?\n"


def build_image(extra: dict[str, str]) -> str | None:
    """Build the console image; an error message, or None."""
    import busybox
    shell = busybox.ensure_busybox()
    if shell is None:
        return "no BusyBox for the console shell (see tools/abi/busybox.py)"
    env = dict(os.environ, LAZYOS_CLI="1", LAZYOS_NETD="1", LAZYOS_NETD_ARGS="demo=0",
               LAZYOS_WEBKIT="1", LAZYOS_RESET_OS="1", LAZYOS_BUSYBOX=str(shell), **extra)
    for stale in ("LAZYOS_DESKTOP", "LAZYOS_TLS", "LAZYOS_JSC", "LAZYOS_LINUXAPPS", "LAZYOS_WEBKIT_DEPS"):
        env.pop(stale, None)
    env.setdefault("LAZYOS_OS_SIZE", "768M")
    print("webkit: cargo build (LAZYOS_WEBKIT=1, console, networking)", flush=True)
    result = subprocess.run(["cargo", "build"], cwd=ROOT, env=env, capture_output=True, text=True)
    if result.returncode != 0:
        return "cargo build failed:\n" + result.stderr[-4000:]
    built = ROOT / "target" / "lazyos.img"
    if not built.is_file():
        return f"{built} was not built"
    IMAGE.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(built, IMAGE)
    return None


def session(command: str, timeout: float) -> list[dict]:
    """The ``qemu_session.py`` steps: wait for the shell, type the command, wait for its last line."""
    return [
        {"wait_for": PROMPT, "timeout": 300},
        {"wait_for": "NETD:RESOLV wrote", "timeout": 300},
        {"type": command, "delay": 0.05},
        {"key": "enter", "until": "WEBKIT:W4:UP", "timeout": timeout},
        {"wait_for": FINISHED, "regex": True, "timeout": timeout},
        {"wait": 1.0},
        {"shot": "webkit-port"},
        {"quit": True},
    ]


def boot(args: argparse.Namespace, out: Path, command: str) -> tuple[bool, str]:
    script = out / "session.json"
    script.write_text(json.dumps(session(command, args.timeout), indent=1))
    cmd = [PY, str(ROOT / "tools" / "screenshot" / "qemu_session.py"), "--image", str(IMAGE),
           "--out", str(out), "--script", str(script), "--accel", args.accel, "--memory", args.memory,
           "--net", "--net-forward", "none", "--fail-on", "PANIC", "--fail-on", "EXCEPTION"]
    if args.qemu:
        cmd += ["--qemu", args.qemu]
    code = subprocess.call(cmd, cwd=ROOT, stdout=subprocess.DEVNULL)
    log = out / "serial.log"
    return code == 0, log.read_text(errors="replace") if log.is_file() else ""


def report(serial: str) -> int:
    host = HOST_RUN.read_text(errors="replace") if HOST_RUN.is_file() else None
    if host is None:
        print("webkit: no host reference run (target/webkit/smoke.host.txt): the paint is not compared")
    verdicts = portjudge.judge(serial, host)
    for verdict in verdicts:
        print(f"{'PASS' if verdict.ok else 'FAIL'}  {verdict.name}  {verdict.detail}".rstrip())
    for name in ("WebProcess", "NetworkProcess", "webkit-smoke"):
        if (BIN / name).is_file():
            print(f"{name}: {(BIN / name).stat().st_size / 1048576:.1f} MiB")
    print(f"stage reached: {portjudge.reached(serial)}")
    ok = portjudge.passed(verdicts)
    print("WEBKIT:W4:HARNESS:" + ("PASS" if ok else "FAIL"))
    return 0 if ok else 1


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--qemu", help="path to qemu-system-x86_64")
    parser.add_argument("--accel", default="auto", choices=["auto", "none", "tcg", "whpx", "kvm"])
    parser.add_argument("--memory", default="4G", help="guest RAM (default 4G)")
    parser.add_argument("--timeout", type=float, default=300.0, help="seconds to wait for the program")
    parser.add_argument("--out", default="shots/webkit-port")
    parser.add_argument("--no-build", action="store_true", help="boot the last image")
    parser.add_argument("--judge-only", metavar="LOG", help="judge a saved serial log and exit")
    args = parser.parse_args()
    out = Path(args.out) if Path(args.out).is_absolute() else ROOT / args.out
    out.mkdir(parents=True, exist_ok=True)

    if args.judge_only:
        return report(Path(args.judge_only).read_text(errors="replace"))
    missing = [n for n in ("WebProcess", "NetworkProcess", "webkit-smoke") if not (BIN / n).is_file()]
    if missing:
        print(f"webkit: {', '.join(missing)} not built (python tools/webkit/port.py)", file=sys.stderr)
        return 1
    if not args.no_build:
        error = build_image({})
        if error:
            print(f"webkit: {error}", file=sys.stderr)
            return 1
    elif not IMAGE.is_file():
        print(f"webkit: {IMAGE} not found", file=sys.stderr)
        return 1
    (out / "serial.log").unlink(missing_ok=True)
    session_ok, serial = boot(args, out, "webkit-smoke")
    if not session_ok:
        print("FAIL  the session did not finish (see summary.json)")
    code = report(serial)
    return code if session_ok else 1


if __name__ == "__main__":
    raise SystemExit(main())
