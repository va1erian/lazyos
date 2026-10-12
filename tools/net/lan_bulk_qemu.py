"""The `--dry-run-qemu` half of `tools/net/lan_bulk.py`: a stand-in for the
person at a real box.

It builds a desktop image with the stack and `dbgd` (a throwaway key, the
peer pinned to QEMU's gateway; the desktop is the only image that has both
`dbgd`, which needs `init`, and a shell), boots it headless on QEMU's user
network (the guest reaches this host as 10.0.2.2, `dbgd`'s 9701 is forwarded
to 127.0.0.1:19701) and types the `netbulk` command into the Terminal, as
the person would. Everything else the tool does is the same code as on the
LAN: the same server, the same reads of the box over `dbgd`, the same
cross-check. The xui apps must be built (`python tools/xui/build.py`).
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent

PY = sys.executable
IMAGE = ROOT / "target" / "lazyos.img"
#: A key for the dry run alone; the real boxes' key is never put in an image.
DRY_KEY = "00112233445566778899aabbccddeeff"
DBGD_GUEST_PORT = 9701
DBGD_HOST_PORT = 19701
#: How the guest sees this host, and what the server then sees of the guest
#: (QEMU's user network connects out from the host's loopback).
GATEWAY = "10.0.2.2"
SERVER_SEES = "127.0.0.1"
#: Seconds the session stays up after `netbulk` is done, for the tool to
#: read the box's log and finish.
LINGER = 90


def build() -> str | None:
    """Build the image; an error message, or None."""
    xui = ROOT / "target" / "xui"
    if not (xui / "xui-term.elf").is_file():
        return f"no xui apps in {xui} (run python tools/xui/build.py)"
    env = dict(os.environ, LAZYOS_DESKTOP="1", LAZYOS_XUI_AUTOSTART="term", LAZYOS_NETD="1",
               LAZYOS_NETD_ARGS="demo=0", LAZYOS_DBGD="1", LAZYOS_DBGD_KEY=DRY_KEY,
               LAZYOS_DBGD_PEER=GATEWAY, LAZYOS_RESET_OS="1")
    print("lan_bulk: LAZYOS_DESKTOP=1 LAZYOS_NETD=1 LAZYOS_DBGD=1 cargo build", file=sys.stderr, flush=True)
    result = subprocess.run(["cargo", "build"], cwd=ROOT, env=env, capture_output=True, text=True)
    if result.returncode != 0:
        return "cargo build failed:\n" + result.stderr[-4000:]
    return None if IMAGE.is_file() else f"{IMAGE} was not built"


def script(command: str) -> list[dict]:
    """The session: wait for the desktop's Terminal and the address, type `command`."""
    return [
        {"wait_for": "PKGD:PROVISION:DONE", "timeout": 420},
        {"wait_for": "TERM:UP:PASS", "timeout": 240},
        {"wait_for": "NETD:ADDR", "timeout": 300},
        {"wait": 1.0},
        {"type": command, "delay": 0.05},
        {"key": "enter", "until": "TERM:CMD:netbulk", "timeout": 60},
        {"wait_for": "TERM:OUT:", "timeout": 900},
        {"wait": LINGER},
        {"quit": True},
    ]


def start(command: str, out: Path, accel: str, qemu: str | None, memory: str | None) -> subprocess.Popen:
    """Boot the image and type `command`; the running session runner."""
    out.mkdir(parents=True, exist_ok=True)
    (out / "serial.log").unlink(missing_ok=True)
    (out / "session.json").write_text(json.dumps(script(command), indent=1))
    argv = [PY, str(ROOT / "tools" / "screenshot" / "qemu_session.py"), "--image", str(IMAGE),
            "--out", str(out), "--script", str(out / "session.json"), "--accel", accel,
            "--net", "--net-forward", f"{DBGD_HOST_PORT}:{DBGD_GUEST_PORT}",
            "--fail-on", "PANIC", "--fail-on", "EXCEPTION"]
    if qemu:
        argv += ["--qemu", qemu]
    if memory:
        argv += ["--memory", memory]
    return subprocess.Popen(argv, cwd=ROOT, stdout=subprocess.DEVNULL)


def stop(proc: subprocess.Popen) -> None:
    """End the session runner and the QEMU it started (its whole tree)."""
    if proc.poll() is not None:
        return
    if os.name == "nt":
        subprocess.run(["taskkill", "/F", "/T", "/PID", str(proc.pid)], capture_output=True, check=False)
    else:
        proc.terminate()
    try:
        proc.wait(timeout=20)
    except subprocess.TimeoutExpired:
        proc.kill()
