#!/usr/bin/env python3
"""Build a desktop with LazySky, point it at a mock Bluesky over TLS, drive
it, and judge what the guest, the mock and the screen saw (docs/lazysky.md).

1. A throwaway test CA and a leaf for `bsky.test` / `cdn.bsky.test`
   (`certs.py`); the image is built with `LAZYOS_LAZYSKY=1` (desktop, `netd`),
   the CA appended to the system bundle (`LAZYOS_TLS_TEST_CA`) and the names
   mapped to the host in /etc/hosts (`LAZYOS_TLS_TEST_HOSTS`): the client
   verifies certificates exactly as in production.
2. The host serves the mock (`mockserver.py`) on 127.0.0.1:8443: sign-in, a
   paged timeline of 400 generated posts, pictures, threads.
3. The session (`session.py`, also `tools/screenshot/examples/lazysky.json`)
   writes the app's `settings.toml` from the Terminal, launches LazySky,
   types the handle and the app password (a random one, from the
   environment), scrolls the feed down and up, clicks a post, goes back.
4. The verdict (`judge.py`): the app's markers; no failure marker; no
   password or token in the serial log; the mock saw the sign-in body, a
   bearer token on every XRPC call, SNI `bsky.test` and `cdn.bsky.test`, the
   pictures; the frame times of `SKY:PERF` within R7; the screenshots differ.

    python tools/lazysky/run.py                # build, boot, judge
    python tools/lazysky/run.py --no-build     # reuse target/lazyos.img and shots/lazysky/certs
    python tools/lazysky/run.py --label-trace  # also LAZYOS_LABEL_TRACE=1: list LABEL:DENY lines

Exit status is non-zero on any failure; logs and screenshots go to `shots/lazysky`.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import secrets
import subprocess
import sys
from dataclasses import asdict
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(ROOT / "tools" / "abi"))
import busybox  # noqa: E402
import certs  # noqa: E402
import judge  # noqa: E402
import mockserver  # noqa: E402
import session  # noqa: E402

PY = sys.executable
IMAGE = ROOT / "target" / "lazyos.img"
ELF = ROOT / "target" / "xui" / "xui-lazysky.elf"
#: Switches a previous build may have left in the caller's environment.
STALE = ("LAZYOS_TLS_TEST_CA", "LAZYOS_TLS_TEST_HOSTS", "LAZYOS_DESKTOP", "LAZYOS_CLI",
         "LAZYOS_LAZYSKY", "LAZYOS_XUI_APPS", "LAZYOS_XUI_AUTOSTART", "LAZYOS_LABEL_TRACE")


def _tool(*argv: str) -> bool:
    return subprocess.call([PY, *argv], cwd=ROOT) == 0


def build(extra: dict[str, str], label_trace: bool) -> str | None:
    """Build the apps, `rhai` and the image; an error message, or None."""
    if not _tool(str(ROOT / "tools" / "xui" / "build.py")):
        return "tools/xui/build.py failed"
    if not ELF.is_file():
        return f"{ELF} was not built (run `python tools/xui/build.py` and read its errors)"
    if not _tool(str(ROOT / "tools" / "rhai" / "build.py")):
        return "tools/rhai/build.py failed"
    shell = busybox.ensure_busybox()
    if shell is None:
        return "no BusyBox for the Terminal (see tools/abi/busybox.py)"
    env = {k: v for k, v in os.environ.items() if k not in STALE}
    env.update(LAZYOS_DESKTOP="1", LAZYOS_NETD="1", LAZYOS_NETD_ARGS="demo=0",
               LAZYOS_LAZYSKY="1", LAZYOS_UI_PROBE="1", LAZYOS_RESET_OS="1",
               # The session types into a Terminal and launches the app from it.
               LAZYOS_XUI_AUTOSTART="term", LAZYOS_BUSYBOX=str(shell), **extra)
    if label_trace:
        env["LAZYOS_LABEL_TRACE"] = "1"
    shown = " ".join(f"{k}={env[k]}" for k in sorted(env)
                     if k.startswith("LAZYOS_") and k != "LAZYOS_BUSYBOX")
    print(f"lazysky: {shown} cargo build", flush=True)
    result = subprocess.run(["cargo", "build"], cwd=ROOT, env=env, capture_output=True, text=True)
    if result.returncode != 0:
        return "cargo build failed:\n" + result.stderr[-4000:]
    return None if IMAGE.is_file() else f"{IMAGE} was not built"


def run_session(out: Path, args, password: str) -> tuple[bool, str]:
    """Drive the guest; (session ok, serial log text)."""
    script = out / "session.json"
    script.write_text(session.render(session.steps(args.step_timeout)), encoding="utf-8")
    command = [PY, str(ROOT / "tools" / "screenshot" / "qemu_session.py"), "--image", str(IMAGE),
               "--out", str(out), "--script", str(script), "--accel", args.accel,
               "--net", "--net-forward", "none", "--net-pcap", str(out / "net.pcap"),
               "--memory", args.memory, "--fail-on", "PANIC", "--fail-on", "EXCEPTION"]
    if args.qemu:
        command += ["--qemu", args.qemu]
    env = dict(os.environ, LAZYSKY_PASSWORD=password)
    with open(out / "session.log", "w", encoding="utf-8") as console:
        code = subprocess.call(command, cwd=ROOT, env=env, stdout=console, stderr=subprocess.STDOUT)
    log = out / "serial.log"
    return code == 0, log.read_text(errors="replace") if log.is_file() else ""


def report(section: str, detail: str, problems: list[str]) -> bool:
    for problem in problems[:16]:
        print(f"LAZYSKY:{section}:FAIL {problem}")
    if not problems:
        print(f"LAZYSKY:{section}:PASS {detail}".rstrip())
    return not problems


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--out", default="shots/lazysky")
    parser.add_argument("--no-build", action="store_true", help="reuse the image (built by this script)")
    parser.add_argument("--label-trace", action="store_true", help="list LABEL:DENY lines")
    parser.add_argument("--accel", default="auto", choices=["auto", "none", "tcg", "whpx", "kvm"])
    parser.add_argument("--qemu")
    parser.add_argument("--memory", default="2G")
    parser.add_argument("--step-timeout", type=float, default=300.0, help="seconds per gate")
    args = parser.parse_args(argv)
    out = Path(args.out) if Path(args.out).is_absolute() else ROOT / args.out
    out.mkdir(parents=True, exist_ok=True)
    for stale in ["serial.log", "net.pcap", "summary.json", *(p.name for p in out.glob("shot_*.png"))]:
        (out / stale).unlink(missing_ok=True)

    files = certs.load(out / "certs") if args.no_build else certs.generate(out / "certs")
    if files is None or (args.no_build and not IMAGE.is_file()):
        print("LAZYSKY:HARNESS:FAIL --no-build needs the certificates and the image of an earlier run")
        return 1
    if not args.no_build:
        error = build({"LAZYOS_TLS_TEST_CA": str(files.ca), "LAZYOS_TLS_TEST_HOSTS": str(files.hosts)},
                      args.label_trace)
        if error:
            print(f"LAZYSKY:HARNESS:FAIL {error}")
            return 1
    password = secrets.token_urlsafe(18)
    try:
        mock = mockserver.Mock(files.leaf_cert, files.leaf_key, mockserver.PORT, "127.0.0.1")
    except OSError as error:
        print(f"LAZYSKY:HARNESS:FAIL cannot start the mock on 127.0.0.1:{mockserver.PORT}: {error}")
        return 1
    try:
        session_ok, text = run_session(out, args, password)
    finally:
        requests = [asdict(r) for r in mock.record.requests]
        mock.close()
    (out / "requests.json").write_text(json.dumps(requests, indent=1), encoding="utf-8")

    ok = report("SESSION", "", [] if session_ok else ["the session did not finish (see summary.json)"])
    ok = report("SERIAL", "markers, no failure marker, no secret", judge.judge_serial(text, password)) and ok
    console = out / "session.log"
    chosen = console.read_text(errors="replace") if console.is_file() else ""
    accelerated = bool(re.search(r"accelerator: \w+ -> (whpx|kvm)", chosen)) or args.accel in ("whpx", "kvm")
    ok = report("PERF", f"{len(judge.perf_lines(text))} reports within R7",
                judge.judge_perf(text, accelerated)) and ok
    ok = report("MOCK", f"{len(requests)} requests as the guest sent them",
                judge.judge_requests(requests, password)) and ok
    shots = sorted(out.glob("shot_*.png"))
    ok = report("SHOTS", ", ".join(p.name for p in shots), judge.judge_shots(shots)) and ok
    denies = sorted(set(re.findall(r"^LABEL:DENY .*$", text, re.M)))
    for line in denies:
        print(f"  {line}")
    if args.label_trace and denies:
        ok = False
    print("LAZYSKY:HARNESS:" + ("PASS" if ok else "FAIL"))
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
