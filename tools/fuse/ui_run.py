#!/usr/bin/env python3
"""Mount an FTP server from the Network Drives app and judge it end to end:
the desktop front end of `ftpfuse` through the network mount service `mountd`
(docs/smb-plan.md §3.4).

1. `ftpserver.py` serves a seeded temporary directory on the host loopback;
   the guest reaches it at 10.0.2.2.
2. `tools/xui/build.py`, then `cargo build` of a desktop with the network
   stack, the Terminal, the UI probe and the label trace
   (`LAZYOS_DESKTOP=1 LAZYOS_NETD=1 LAZYOS_UI_PROBE=1 LAZYOS_LABEL_TRACE=1`).
3. A session starts Network Drives from the Terminal and fills its form by
   widget name: a wrong password first (the mount must fail with the login
   reason), then the right one (it must mount at /mnt/site). It clears the
   failed row, reads a file through the mount from the Terminal, writes one,
   opens the folder in Files, then unmounts it.
4. The verdict: the app's and `mountd`'s markers, the file the guest wrote as
   the server stored it, the logins the server saw, and no `LABEL:DENY` for
   the app (its manifest permissions are complete) or for `mountd`.

    python tools/fuse/ui_run.py               # build, boot, judge
    python tools/fuse/ui_run.py --no-build    # reuse target/lazyos.img

Logs, the generated session and the screenshots go to `shots/netdrives/`.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
sys.path.insert(0, str(ROOT / "tools" / "abi"))
sys.path.insert(0, str(HERE))
import busybox  # noqa: E402
import ftpserver  # noqa: E402

PY = sys.executable
SESSION = ROOT / "tools" / "screenshot" / "qemu_session.py"
IMAGE = ROOT / "target" / "lazyos.img"
OUT = ROOT / "shots" / "netdrives"
USER, PASSWORD = "lazy", "os"
WINDOW = "Network Drives"
#: What the guest writes through the mount.
WRITTEN = b"written by lazyos\n"
FAIL_ON = (r"FTPFUSE:FAIL panic", r"MOUNTD:FAIL panic", r"user: task [0-9]+ killed by", r"panicked")


def fail(message: str) -> int:
    print(f"NETDRIVES:HARNESS:FAIL: {message}", file=sys.stderr)
    return 1


def seed(root: Path) -> None:
    (root / "hello.txt").write_bytes(b"hello from the host\n")
    (root / "pub").mkdir()
    (root / "pub" / "readme.txt").write_bytes(b"read me\n")


def fill(widget: str, text: str) -> list[dict]:
    """Click a form field by its probe name, select what it holds, type."""
    return [{"click_at": {"window": WINDOW, "widget": widget}, "timeout": 30},
            {"key_down": "ctrl"}, {"key": "a"}, {"key_up": "ctrl"},
            {"type": text}]


def click(widget: str, until: str, timeout: int = 60) -> list[dict]:
    return [{"click_at": {"window": WINDOW, "widget": widget}, "timeout": 30},
            {"wait_for": until, "timeout": timeout}]


def terminal(command: str, until: str, timeout: int = 60) -> list[dict]:
    """Run `command` in the Terminal and wait for its output. The click lands
    near the Terminal's left edge, which the app's window (opened to its
    right) leaves uncovered."""
    # The first key after the focusing click is lost without the pause.
    return [{"click_at": {"window": "Terminal", "offset": [-300, 0]}, "timeout": 30},
            {"wait": 1.0},
            {"type": command},
            {"key": "enter", "until": until, "timeout": timeout, "retries": 1}]


def session(port: int) -> list[dict]:
    steps: list[dict] = [
        {"wait_for": "PKGD:PROVISION:DONE", "timeout": 420},
        {"wait_for": "TERM:UP:PASS", "timeout": 120},
        {"wait_for": "MOUNTD:READY", "timeout": 120},
        {"wait_for": "NETD:ADDR 10.0.2.15/24", "timeout": 120},
        {"at": 1.0, "type": "PS1='# '; echo 'let i = msg::connect(\"os.lazy.init.v1\");' > /tmp/l.rhai"},
        {"key": "enter"},
        {"at": 1.0, "type": "echo 'for a in os::args() { print(i.launch(a, \"\", 0)); }' >> /tmp/l.rhai"},
        {"key": "enter"},
        {"at": 1.0, "type": "clear; rhai /tmp/l.rhai os.lazy.netdrives"},
        {"key": "enter"},
        {"wait_for": "NETDRIVES:UP:PASS", "timeout": 120},
        {"wait_for": f"name=unmount_button window={WINDOW}", "timeout": 30},
        {"shot": "01_empty"},
    ]
    # A wrong password: the daemon starts, the login fails, the row says why.
    steps += fill("host", "10.0.2.2") + fill("port", str(port)) + fill("user", USER)
    steps += fill("password", "wrong") + fill("name", "bad")
    steps += click("mount_button", "NETDRIVES:MOUNT:FAIL name=bad", 120)
    steps += [{"wait": 1.0}, {"shot": "02_login_refused"}]
    # The failed row is the only one, so it is selected: clear it.
    steps += click("unmount_button", "NETDRIVES:UNMOUNT:PASS name=bad")
    steps += fill("password", PASSWORD) + fill("name", "site")
    steps += click("mount_button", "NETDRIVES:MOUNT:PASS name=site path=/mnt/site", 120)
    steps += [{"wait": 1.0}, {"shot": "03_mounted"}]
    # Through the ordinary VFS, from the Terminal.
    steps += terminal("cat /mnt/site/hello.txt", "TERM:OUT:hello from the host")
    steps += terminal("echo 'written by lazyos' > /mnt/site/fromguest.txt; echo WROTE:$?", "TERM:OUT:WROTE:0")
    steps += terminal("stat -c OWNER:%u:%g /mnt/site/hello.txt", "TERM:OUT:OWNER:")
    # Files opens the mounted folder.
    steps += click("open_button", "NETDRIVES:OPEN:PASS path=/mnt/site")
    steps += [{"wait_for": "FILES:UP:PASS", "timeout": 60}, {"wait": 3.0}, {"shot": "04_files"}]
    # Files opens over the app's buttons: close it (Alt+F4, it has the focus).
    steps += [{"key_down": "alt"}, {"key": "f4"}, {"key_up": "alt"}, {"wait": 2.0}]
    steps += click("unmount_button", "NETDRIVES:UNMOUNT:PASS name=site")
    steps += [{"wait_for": "MOUNTD:STOP site", "timeout": 30}, {"wait": 8.0}]
    steps += terminal("ls /mnt/site >/dev/null 2>&1; echo GONE:$?", "TERM:OUT:GONE:")
    steps += [{"shot": "05_unmounted"}, {"quit": True}]
    return steps


def build() -> str | None:
    shell = os.environ.get("LAZYOS_BUSYBOX")
    if not (shell and Path(shell).is_file()):
        found = busybox.ensure_busybox()
        if found is None:
            return "no BusyBox: run `python tools/abi/busybox.py`, or set LAZYOS_BUSYBOX"
        shell = str(found)
    if subprocess.call([PY, "tools/xui/build.py", "--no-lazyweb"], cwd=ROOT) != 0:
        return "tools/xui/build.py failed"
    # The session starts the app with a `rhai` script (`init.Launch`).
    if subprocess.call([PY, "tools/rhai/build.py"], cwd=ROOT) != 0:
        return "tools/rhai/build.py failed"
    env = dict(os.environ, LAZYOS_BUSYBOX=shell, LAZYOS_RESET_OS="1", LAZYOS_DESKTOP="1",
               LAZYOS_XUI_AUTOSTART="term", LAZYOS_NETD="1", LAZYOS_NETD_ARGS="demo=0",
               LAZYOS_UI_PROBE="1", LAZYOS_LABEL_TRACE="1")
    print("netdrives: desktop + netd + UI probe + label trace: cargo build", flush=True)
    result = subprocess.run(["cargo", "build"], cwd=ROOT, env=env, capture_output=True, text=True)
    if result.returncode != 0:
        sys.stderr.write(result.stderr[-4000:])
        return "cargo build failed"
    return None if IMAGE.is_file() else f"{IMAGE} was not written"


#: Markers the session must have produced, in words for a failure.
EXPECT = (
    (r"NETDRIVES:MOUNT:FAIL name=bad reason=cannot connect or log in", "the wrong password did not fail the login"),
    (r"MOUNTD:START site pid=\d+", "mountd never started the daemon"),
    (r"NETDRIVES:MOUNT:PASS name=site path=/mnt/site", "the mount never came up"),
    (r"FTPFUSE:UP /mnt/site", "ftpfuse never mounted /mnt/site"),
    (r"TERM:OUT:hello from the host", "the Terminal could not read through the mount"),
    (r"NETDRIVES:OPEN:PASS path=/mnt/site", "Files was not opened"),
    (r"NETDRIVES:UNMOUNT:PASS name=site", "the unmount failed"),
)


def judge(text: str, root: Path, logins: int) -> list[str]:
    problems = [why for pattern, why in EXPECT if not re.search(pattern, text)]
    # The daemon runs as `_mountd`, but the files are the requester's (root,
    # on this desktop), not 907's.
    if "TERM:OUT:OWNER:0:0" not in text:
        problems.append("the mounted files are not owned by the user who asked for the mount")
    gone = re.search(r"TERM:OUT:GONE:(\d+)", text)
    if not gone or gone.group(1) == "0":
        problems.append("/mnt/site still answers after the unmount")
    for denied in sorted(set(re.findall(r"LABEL:DENY label=app:os\.lazy\.netdrives\S* .*", text))):
        problems.append(f"permission missing from the manifest: {denied}")
    stored = (root / "fromguest.txt").read_bytes() if (root / "fromguest.txt").is_file() else None
    if stored != WRITTEN:
        problems.append(f"the server stored fromguest.txt as {stored!r}, wanted {WRITTEN!r}")
    if logins < 2:
        problems.append(f"the server saw {logins} login(s), wanted the refused one and the accepted one")
    # `ftpfuse`'s argv, or an FTP `PASS` echoed with its argument.
    if re.search(r"pass=(wrong|os)\b|\bPASS wrong\b", text):
        problems.append("a password reached the serial log")
    return problems


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--no-build", action="store_true", help="reuse target/lazyos.img")
    parser.add_argument("--accel", default="auto", choices=["auto", "none", "tcg", "whpx", "kvm"])
    parser.add_argument("--qemu", help="path to qemu-system-x86_64")
    parser.add_argument("--timeout", type=float, default=1200.0, help="seconds for the session")
    args = parser.parse_args()

    if not args.no_build:
        error = build()
        if error:
            return fail(error)
    OUT.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory() as served:
        root = Path(served)
        seed(root)
        server = ftpserver.FtpServer(root, user=USER, password=PASSWORD)
        script = OUT / "session.json"
        script.write_text(json.dumps(session(server.port), indent=1))
        command = [PY, str(SESSION), "--image", str(IMAGE), "--out", str(OUT), "--timeout", str(args.timeout),
                   "--script", str(script), "--accel", args.accel, "--net"]
        for pattern in FAIL_ON:
            command += ["--fail-on", pattern]
        if args.qemu:
            command += ["--qemu", args.qemu]
        print(f"netdrives: server on 127.0.0.1:{server.port}, session {script}", flush=True)
        try:
            code = subprocess.call(command, cwd=ROOT)
        finally:
            server.close()
        log = OUT / "serial.log"
        text = log.read_text(errors="replace") if log.is_file() else ""
        with server._lock:
            logins = sum(1 for verb, _ in server.commands if verb == "PASS")
        problems = judge(text, root, logins)
    if code != 0:
        problems.append(f"the session exited with {code}")
    if problems:
        for problem in problems:
            print(f"netdrives: {problem}", file=sys.stderr)
        return fail(f"{len(problems)} problem(s); see {OUT}/serial.log")
    print("NETDRIVES:HARNESS:PASS")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
