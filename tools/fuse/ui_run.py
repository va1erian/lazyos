#!/usr/bin/env python3
"""Mount an FTP server (or, with `--smb`, an SMB share) from the Network
Drives app and judge it end to end: the desktop front end of `ftpfuse` and
`smbfuse` through the network mount service `mountd` (docs/smb-plan.md §3.4,
F3).

1. `ftpserver.py` (or `tools/smb/smbserver.py`) serves a seeded temporary
   directory on the host loopback; the guest reaches it at 10.0.2.2.
2. `tools/xui/build.py`, then `cargo build` of a desktop with the network
   stack, the Terminal, the UI probe and the label trace
   (`LAZYOS_DESKTOP=1 LAZYOS_NETD=1 LAZYOS_UI_PROBE=1 LAZYOS_LABEL_TRACE=1`).
3. A session starts Network Drives from the Terminal and fills its form by
   widget name (for SMB: the SMB choice and the share too): a wrong password
   first (the mount must fail with the login reason), then the right one (it
   must mount at /mnt/site; for SMB the password is typed from the host
   environment, never written in the script). It clears the
   failed row, reads a file through the mount from the Terminal, writes one,
   opens the folder in Files, then unmounts it.
4. The verdict: the app's and `mountd`'s markers, the file the guest wrote as
   the server stored it, the logins the server saw, and no `LABEL:DENY` for
   the app (its manifest permissions are complete) or for `mountd`.

    python tools/fuse/ui_run.py               # build, boot, judge
    python tools/fuse/ui_run.py --smb         # the same with an SMB share
    python tools/fuse/ui_run.py --no-build    # reuse target/lazyos.img

Logs, the generated session and the screenshots go to `shots/netdrives/`
(`shots/netdrives-smb/`).
"""

from __future__ import annotations

import argparse
import json
import os
import re
import secrets
import string
import subprocess
import sys
import tempfile
from dataclasses import dataclass
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
sys.path.insert(0, str(ROOT / "tools" / "abi"))
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(ROOT / "tools" / "smb"))
import busybox  # noqa: E402
import ftpserver  # noqa: E402
import smbserver  # noqa: E402

PY = sys.executable
SESSION = ROOT / "tools" / "screenshot" / "qemu_session.py"
IMAGE = ROOT / "target" / "lazyos.img"
OUT = ROOT / "shots" / "netdrives"
USER, PASSWORD = "lazy", "os"
WINDOW = "Network Drives"
#: The host variable an SMB run's password is typed from.
SECRET_VAR = "LAZYOS_NETDRIVES_SECRET"


@dataclass(frozen=True)
class Proto:
    """What differs between an FTP and an SMB run."""
    kind: str
    daemon: str
    #: The form's extra fields: (widget, text).
    extra: tuple[tuple[str, str], ...] = ()


FTP = Proto("ftp", "FTPFUSE")
SMB = Proto("smb", "SMBFUSE", (("share", "share"),))
#: What the guest writes through the mount.
WRITTEN = b"written by lazyos\n"
FAIL_ON = (r"FTPFUSE:FAIL panic", r"SMBFUSE:FAIL panic", r"MOUNTD:FAIL panic", r"user: task [0-9]+ killed by",
           r"panicked")


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


def session(port: int, proto: Proto = FTP) -> list[dict]:
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
    if proto is SMB:
        steps += click("kind_smb", "NETDRIVES:KIND smb")
    steps += fill("host", "10.0.2.2") + fill("port", str(port)) + fill("user", USER)
    for widget, text in proto.extra:
        steps += fill(widget, text)
    steps += fill("password", "wrong") + fill("name", "bad")
    steps += click("mount_button", "NETDRIVES:MOUNT:FAIL name=bad", 120)
    steps += [{"wait": 1.0}, {"shot": "02_login_refused"}]
    # The failed row is the only one, so it is selected: clear it.
    steps += click("unmount_button", "NETDRIVES:UNMOUNT:PASS name=bad")
    if proto is SMB:
        steps += fill("password", "")[:-1] + [{"type_secret": SECRET_VAR}] + fill("name", "site")
    else:
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


def expectations(proto: Proto) -> tuple[tuple[str, str], ...]:
    """Markers the session must have produced, in words for a failure."""
    daemon = proto.daemon.lower()
    return (
        (r"NETDRIVES:MOUNT:FAIL name=bad reason=cannot connect or log in",
         "the wrong password did not fail the login"),
        (rf"MOUNTD:START site kind={proto.kind} pid=\d+", "mountd never started the daemon"),
        (r"NETDRIVES:MOUNT:PASS name=site path=/mnt/site", "the mount never came up"),
        (rf"{proto.daemon}:UP /mnt/site", f"{daemon} never mounted /mnt/site"),
        (r"TERM:OUT:hello from the host", "the Terminal could not read through the mount"),
        (r"NETDRIVES:OPEN:PASS path=/mnt/site", "Files was not opened"),
        (r"NETDRIVES:UNMOUNT:PASS name=site", "the unmount failed"),
    )


EXPECT = expectations(FTP)


def judge(text: str, root: Path, logins: int, proto: Proto = FTP, secret: str = PASSWORD) -> list[str]:
    problems = [why for pattern, why in expectations(proto) if not re.search(pattern, text)]
    # The daemon runs as `_mountd`, but the files are the requester's: the
    # session user (`user`, 1000), not 910's.
    if "TERM:OUT:OWNER:1000:1000" not in text:
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
    if re.search(rf"pass=(wrong|{re.escape(secret)})\b|\bPASS wrong\b", text):
        problems.append("a password reached the serial log")
    if proto is SMB and secret in text:
        problems.append("the SMB password reached the serial log")
    return problems


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--no-build", action="store_true", help="reuse target/lazyos.img")
    parser.add_argument("--accel", default="auto", choices=["auto", "none", "tcg", "whpx", "kvm"])
    parser.add_argument("--qemu", help="path to qemu-system-x86_64")
    parser.add_argument("--timeout", type=float, default=1200.0, help="seconds for the session")
    parser.add_argument("--smb", action="store_true", help="mount an SMB share instead of an FTP server")
    args = parser.parse_args()

    proto = SMB if args.smb else FTP
    out = OUT.with_name("netdrives-smb") if args.smb else OUT
    secret = "".join(secrets.choice(string.ascii_letters + string.digits) for _ in range(16)) \
        if args.smb else PASSWORD
    if not args.no_build:
        error = build()
        if error:
            return fail(error)
    out.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory() as served:
        root = Path(served)
        seed(root)
        if args.smb:
            server = smbserver.SmbServer(root, 0, smbserver.Options(user=USER, password=secret))
        else:
            server = ftpserver.FtpServer(root, user=USER, password=PASSWORD)
        script = out / "session.json"
        script.write_text(json.dumps(session(server.port, proto), indent=1))
        command = [PY, str(SESSION), "--image", str(IMAGE), "--out", str(out), "--timeout", str(args.timeout),
                   "--script", str(script), "--accel", args.accel, "--net"]
        for pattern in FAIL_ON:
            command += ["--fail-on", pattern]
        if args.qemu:
            command += ["--qemu", args.qemu]
        print(f"netdrives: server on 127.0.0.1:{server.port}, session {script}", flush=True)
        try:
            code = subprocess.call(command, cwd=ROOT, env=dict(os.environ, **{SECRET_VAR: secret}))
        finally:
            server.close()
        log = out / "serial.log"
        text = log.read_text(errors="replace") if log.is_file() else ""
        if args.smb:
            logins = len(server.record.logons)
        else:
            with server._lock:
                logins = sum(1 for verb, _ in server.commands if verb == "PASS")
        problems = judge(text, root, logins, proto, secret)
    if code != 0:
        problems.append(f"the session exited with {code}")
    if problems:
        for problem in problems:
            print(f"netdrives: {problem}", file=sys.stderr)
        return fail(f"{len(problems)} problem(s); see {out}/serial.log")
    print("NETDRIVES:HARNESS:PASS")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
