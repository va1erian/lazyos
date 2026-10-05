#!/usr/bin/env python3
"""Mount `memfuse` in a booted LazyOS and use it from the shell: the end-to-end
check of the FUSE mechanism (docs/smb-plan.md stage F1).

1. BusyBox, the shell (`tools/abi/busybox.py`; a git worktree reuses the main
   checkout's cached build);
2. `cargo build` with `LAZYOS_CLI=1` (console boot, root shell; `memfuse` is
   on every image);
3. a headless QEMU session typing `tools/screenshot/examples/fuse_memfuse.json`:
   start `memfuse` (it mounts `/mnt/mem` through syscall 35), write, read,
   `cp` a 400 KB file in and out and compare it byte for byte with `cmp`,
   rename, append, remove, `statfs`, `touch`; then kill the daemon (its files
   fail at once, the flusher unmounts it) and mount it again.

Every BusyBox applet here is an ordinary Linux program going through the
kernel VFS; none of them knows the files live in another task. The session
prints `FUSE:<check>:PASS|FAIL` markers and both `md5sum`s.

    python tools/fuse/run.py               # build, boot, judge
    python tools/fuse/run.py --no-build    # reuse target/lazyos.img
    python tools/fuse/run.py --accel none  # force TCG

Exit status is non-zero on any failure; the log and a screenshot go to
`shots/fuse/`.
"""

from __future__ import annotations

import argparse
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
SCRIPT = ROOT / "tools" / "screenshot" / "examples" / "fuse_memfuse.json"
IMAGE = ROOT / "target" / "lazyos.img"
OUT = "shots/fuse"
#: Every check the session reports, in order.
CHECKS = (
    "mounted", "echo", "ls", "size", "cpin", "cpout", "rename", "renamed", "append",
    "remove", "statfs", "touch", "dead", "reaped", "remount", "remounted", "done",
)
#: Lines that fail the run as soon as they appear.
FAIL_ON = (r"FUSE:[a-z]+:FAIL", r"MEMFUSE:FAIL", r"user: task [0-9]+ killed by", r"panicked")


def fail(message: str) -> int:
    print(f"FUSE:HARNESS:FAIL: {message}", file=sys.stderr)
    return 1


def build() -> str | None:
    """Build the console image; an error message, or None."""
    shell = os.environ.get("LAZYOS_BUSYBOX")
    if not (shell and Path(shell).is_file()):
        found = busybox.ensure_busybox()
        if found is None:
            return ("no BusyBox: run `python tools/abi/busybox.py` (Linux with musl-gcc, "
                    "or Docker), or set LAZYOS_BUSYBOX to a static busybox")
        shell = str(found)
    env = dict(os.environ, LAZYOS_BUSYBOX=shell, LAZYOS_CLI="1")
    print("fuse: LAZYOS_CLI=1 cargo build", flush=True)
    result = subprocess.run(["cargo", "build"], cwd=ROOT, env=env, capture_output=True, text=True)
    if result.returncode != 0:
        sys.stderr.write(result.stderr[-4000:])
        return "cargo build (LAZYOS_CLI=1) failed"
    return None if IMAGE.is_file() else f"{IMAGE} was not written"


def judge(text: str) -> list[str]:
    """The problems in a session's serial log (empty: it passed)."""
    problems = []
    seen = dict(re.findall(r"^FUSE:([a-z]+):(PASS|FAIL\S*)", text, re.M))
    for check in CHECKS:
        state = seen.get(check, "missing")
        print(f"  {check:10} {state}")
        if state != "PASS":
            problems.append(f"check {check}: {state}")
    if "MEMFUSE:UP /mnt/mem" not in text:
        problems.append("memfuse never reported its mount")
    # The two digests of the 400 KB file, original and through the mount.
    digests = re.findall(r"^([0-9a-f]{32})\s+-?\s*$", text, re.M)
    if len(digests) < 2 or digests[0] != digests[1]:
        problems.append(f"md5 digests differ or are missing: {digests}")
    if "fuse: /mnt/mem mounted by task" not in text:
        problems.append("the kernel did not log the mount")
    if "fuse: /mnt/mem unmounted" not in text:
        problems.append("the kernel did not unmount the dead daemon")
    return problems


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--no-build", action="store_true", help="reuse target/lazyos.img")
    parser.add_argument("--accel", default="auto", choices=["auto", "none", "tcg", "whpx", "kvm"])
    parser.add_argument("--qemu", help="path to qemu-system-x86_64")
    parser.add_argument("--memory", help="guest RAM (default: the session tool's, 1G)")
    parser.add_argument("--timeout", type=float, default=600.0, help="seconds for the session")
    args = parser.parse_args()

    if not args.no_build:
        error = build()
        if error:
            return fail(error)
    command = [PY, str(SESSION), "--image", str(IMAGE), "--out", OUT, "--timeout", str(args.timeout),
               "--script", str(SCRIPT), "--accel", args.accel]
    for pattern in FAIL_ON:
        command += ["--fail-on", pattern]
    if args.qemu:
        command += ["--qemu", args.qemu]
    if args.memory:
        command += ["--memory", args.memory]
    print("fuse: session fuse_memfuse.json", flush=True)
    code = subprocess.call(command, cwd=ROOT)
    log = ROOT / OUT / "serial.log"
    text = log.read_text(errors="replace") if log.is_file() else ""
    problems = judge(text)
    if code != 0:
        problems.append(f"the session exited with {code}")
    if problems:
        for problem in problems:
            print(f"fuse: {problem}", file=sys.stderr)
        return fail(f"{len(problems)} problem(s); see {OUT}/serial.log")
    print("FUSE:HARNESS:PASS")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
