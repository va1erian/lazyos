#!/usr/bin/env python3
"""Boot a developer-tool desktop with networking, run a script in the Playground that
calls every service a non-shell tool must be refused, and judge the serial log.

The guest half of docs/devtool-packages-plan.md section 9, finding 2 and item "coverage":
`tools/accounts/run.py` attacks only from the Terminal, a *shell* tool with the credential
denies lifted. The Playground (`tool:os.lazy.playground`) keeps them, plus the network stack
and mounts when `network` is not declared. The session
`tools/screenshot/examples/playground_refused_services.json` calls `sys::keyd`, `keyd_appsecret`,
`accounts`, `pkgd`, `net_stack` (resolve and ping) and `mount` from a Playground script, then
posts a toast from a second script (a tool's script is identified by its label).
`refused_judge.py` fails unless the kernel's `LABEL:DENY` line for each interface is there,
`pkgd` refused the install itself, nothing was created, and the toast was posted.

    python tools/devtool/refused_run.py                # build (run_demo --devtools --net), session, judge
    python tools/devtool/refused_run.py --no-build     # reuse target/lazyos.img
    python tools/devtool/refused_run.py --judge-only DIR   # judge DIR/serial.log of an earlier run

Read the screenshots in `shots/devtool_refused/`: seven "refused" lines, then `toast-posted`.
Exit status is non-zero on any failure. `python tools/devtool/test_refused_judge.py` tests the judge.
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
import refused_judge  # noqa: E402

PY = sys.executable
SESSION = ROOT / "tools" / "screenshot" / "qemu_session.py"
SCRIPT = ROOT / "tools" / "screenshot" / "examples" / "playground_refused_services.json"
IMAGE = ROOT / "target" / "lazyos.img"
OUT = ROOT / "shots" / "devtool_refused"

#: The image: the Playground's (`tools/playground/run.py`) plus the network stack, which
#: `--net` brings with `mountd`, so the `os.lazy.net.stack.v1` and `os.lazy.mount.v1` services
#: exist to be refused.
BUILD_ENV = {
    "LAZYOS_UI_PROBE": "1",
    "LAZYOS_XUI_AUTOSTART": "playground",
    "LAZYOS_LABEL_TRACE": "1",
    "LAZYOS_RESET_OS": "1",
}


def fail(message: str) -> int:
    print(f"DEVTOOL:REFUSED:FAIL: {message}", file=sys.stderr)
    return 1


def build() -> bool:
    for step in ([PY, str(ROOT / "tools" / "rhai" / "build.py")], [PY, str(ROOT / "tools" / "xui" / "build.py")]):
        print(f"devtool_refused: {' '.join(step[1:])}", flush=True)
        if subprocess.run(step, cwd=ROOT, capture_output=True, text=True).returncode != 0:
            return False
    command = [PY, str(ROOT / "tools" / "run_demo.py"), "--devtools", "--net", "--autologin", "user", "--build-only"]
    print("devtool_refused: run_demo --devtools --net " + " ".join(f"{k}={v}" for k, v in BUILD_ENV.items()), flush=True)
    result = subprocess.run(command, cwd=ROOT, env=dict(os.environ, **BUILD_ENV), capture_output=True, text=True)
    if result.returncode != 0:
        sys.stderr.write(result.stderr[-4000:])
        return False
    return IMAGE.is_file()


def run_session(args: argparse.Namespace) -> int:
    command = [
        PY, str(SESSION), "--image", str(IMAGE), "--net", "--out", str(OUT), "--timeout", str(args.timeout),
        "--script", str(SCRIPT), "--accel", args.accel,
        "--fail-on", r"PLAYGROUND:(BIND|RUN):FAIL", "--fail-on", "LazyOS PANIC",
    ]
    if args.qemu:
        command += ["--qemu", args.qemu]
    if args.memory:
        command += ["--memory", args.memory]
    return subprocess.call(command, cwd=ROOT)


def judge(directory: Path) -> list[str]:
    log = directory / "serial.log"
    if not log.is_file():
        return [f"no serial log in {directory}"]
    ids = refused_judge.interface_ids(refused_judge.MANIFEST.read_text(encoding="utf-8"))
    return refused_judge.judge_serial(log.read_text(errors="replace"), ids)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--no-build", action="store_true", help="reuse target/lazyos.img")
    parser.add_argument("--judge-only", type=Path, metavar="DIR", help="judge DIR/serial.log and exit")
    parser.add_argument("--accel", default="auto", choices=["auto", "none", "tcg", "whpx", "kvm"])
    parser.add_argument("--timeout", type=int, default=900, help="seconds the session may take")
    parser.add_argument("--qemu", help="path to qemu-system-x86_64")
    parser.add_argument("--memory", help="guest RAM (default: the tools' 1G)")
    args = parser.parse_args(argv)
    if args.judge_only:
        directory = args.judge_only
    else:
        if not args.no_build and not build():
            return fail("the build failed")
        if not IMAGE.is_file():
            return fail(f"{IMAGE} does not exist (build first)")
        code = run_session(args)
        if code != 0:
            return fail(f"the session exited with {code}")
        directory = OUT
    problems = judge(directory)
    for problem in problems:
        print(f"DEVTOOL:REFUSED:FAIL: {problem}", file=sys.stderr)
    if not problems:
        print("DEVTOOL:REFUSED:PASS")
    return 1 if problems else 0


if __name__ == "__main__":
    raise SystemExit(main())
