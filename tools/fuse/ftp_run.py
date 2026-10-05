#!/usr/bin/env python3
"""Mount an FTP server in a booted LazyOS with `ftpfuse` and use it from the
shell: the proof of concept of a network filesystem on the FUSE mechanism.

1. A small FTP server (`ftpserver.py`) on the host loopback serves a seeded
   temporary directory; the guest reaches it at 10.0.2.2 (QEMU's user network).
2. `cargo build` with `LAZYOS_CLI=1 LAZYOS_NETD=1` (console, network stack).
3. A headless session starts `ftpfuse` and works through `/mnt/ftp` with
   BusyBox: list, read (a name with a space, a 300 KB file by `md5sum`), `cp`
   a 289 KB file in, append with `>>`, patch two bytes in place with `dd
   conv=notrunc` (FTP has no random-access write: the daemon rewrites the
   file), `mkdir`, `mv` into it, `rm`, `rmdir`.
4. The verdict is the server's directory afterwards, byte for byte, plus the
   FTP verbs the server saw (APPE, RNFR/RNTO, MKD, DELE, RMD, and `MLSD`, or
   `LIST` with `--list`), plus the guest's `FTPFUSE:<check>:PASS` markers.

    python tools/fuse/ftp_run.py               # build, boot, judge
    python tools/fuse/ftp_run.py --list        # server refuses MLSD: LIST fallback
    python tools/fuse/ftp_run.py --no-build    # reuse target/lazyos.img

Logs, the generated session and a screenshot go to `shots/ftpfuse/`.
"""

from __future__ import annotations

import argparse
import hashlib
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
OUT = ROOT / "shots" / "ftpfuse"
USER, PASSWORD = "lazy", "os"
#: What the guest's `seq 1 50000` writes, regenerated here to compare.
UPLOAD = "".join(f"{n}\n" for n in range(1, 50001)).encode()
BIG = bytes((i * 131 + (i >> 9)) & 0xFF for i in range(300_000))
CHECKS = ("listed", "read", "spaced", "bigsize", "cpin", "cmpin", "append", "patch",
          "rename", "replace", "twin", "rmdir", "gone", "done")
#: Times the server may send big.bin: the twin `cmp` reads it alongside
#: big2.bin, and a cache that held one file would fetch it per 64 KiB read.
MAX_BIG_RETRS = 3
FAIL_ON = (r"FTPFUSE:[a-z]+:FAIL", r"FTPFUSE:FAIL", r"user: task [0-9]+ killed by", r"panicked")


def fail(message: str) -> int:
    print(f"FTPFUSE:HARNESS:FAIL: {message}", file=sys.stderr)
    return 1


def seed(root: Path) -> None:
    (root / "hello.txt").write_bytes(b"hello from the host\n")
    (root / "two words.txt").write_bytes(b"spaced\n")
    (root / "big.bin").write_bytes(BIG)
    (root / "pub").mkdir()
    (root / "pub" / "readme.txt").write_bytes(b"read me\n")
    (root / "old.txt").write_bytes(b"old\n")


def session(port: int, verbose: bool = False) -> list[dict]:
    """The guest's steps, with the server's port."""
    m = "/mnt/ftp"

    def step(command: str, until: str, timeout: int = 120) -> list[dict]:
        return [{"type": command}, {"key": "enter", "until": until, "timeout": timeout, "retries": 1}]

    helpers = ("m=FTPFUSE; ok() { if grep -qxF -- \"$1\"; then echo $m:$2:PASS; else echo $m:$2:FAIL; fi; }; "
               "chk() { if [ \"$2\" = \"$3\" ]; then echo $m:$1:PASS; else echo $m:$1:FAIL:$2; fi; }")
    steps: list[dict] = [{"wait_for": "/ #", "timeout": 300}]
    steps += step(helpers, "/ #", 60)
    # Wait for the mount itself, not a fixed time: on a cold boot `netd` may
    # still be getting its address when the prompt appears.
    flag = "-v " if verbose else ""
    steps += [{"type": f"ftpfuse {flag}10.0.2.2:{port} user={USER} pass={PASSWORD} &"},
              {"key": "enter", "until": "FTPFUSE:UP /mnt/ftp", "timeout": 180, "retries": 1}]
    steps += step(f"ls {m} | ok hello.txt listed", "FTPFUSE:listed:")
    steps += step(f"cat {m}/hello.txt | ok 'hello from the host' read; cat '{m}/two words.txt' | ok spaced spaced",
                  "FTPFUSE:spaced:")
    steps += step(f"stat -c %s {m}/big.bin | ok {len(BIG)} bigsize; md5sum < {m}/big.bin", "FTPFUSE:bigsize:", 180)
    steps += step(f"seq 1 50000 > /tmp/up; cp /tmp/up {m}/up.txt; chk cpin $? 0; cmp /tmp/up {m}/up.txt; chk cmpin $? 0",
                  "FTPFUSE:cmpin:", 300)
    steps += step(f"echo appended >> {m}/hello.txt; tail -n1 {m}/hello.txt | ok appended append", "FTPFUSE:append:")
    steps += step(f"printf HE | dd of={m}/hello.txt bs=1 seek=0 conv=notrunc 2>/dev/null; head -n1 {m}/hello.txt "
                  "| ok 'HEllo from the host' patch", "FTPFUSE:patch:")
    steps += step(f"mkdir {m}/newdir && mv {m}/up.txt {m}/newdir/; ls {m}/newdir | ok up.txt rename", "FTPFUSE:rename:")
    # Rename over an existing file (the editor's save): it is replaced.
    steps += step(f"echo new > {m}/new.part && mv {m}/new.part {m}/old.txt; cat {m}/old.txt | ok new replace",
                  "FTPFUSE:replace:")
    # Two files read in alternation, both through the mount.
    steps += step(f"cp {m}/big.bin {m}/big2.bin && cmp {m}/big.bin {m}/big2.bin; chk twin $? 0",
                  "FTPFUSE:twin:", 300)
    steps += step(f"rm {m}/pub/readme.txt && rmdir {m}/pub; chk rmdir $? 0; ls {m}/pub 2>/dev/null; chk gone $? 1",
                  "FTPFUSE:gone:")
    steps += step("echo $m:done:PASS", "FTPFUSE:done:PASS", 60)
    steps += [{"shot": "ftpfuse"}, {"quit": True}]
    return steps


def build() -> str | None:
    shell = os.environ.get("LAZYOS_BUSYBOX")
    if not (shell and Path(shell).is_file()):
        found = busybox.ensure_busybox()
        if found is None:
            return ("no BusyBox: run `python tools/abi/busybox.py` (Linux with musl-gcc, "
                    "or Docker), or set LAZYOS_BUSYBOX to a static busybox")
        shell = str(found)
    env = dict(os.environ, LAZYOS_BUSYBOX=shell, LAZYOS_CLI="1", LAZYOS_NETD="1", LAZYOS_NETD_ARGS="demo=0")
    print("ftpfuse: LAZYOS_CLI=1 LAZYOS_NETD=1 cargo build", flush=True)
    result = subprocess.run(["cargo", "build"], cwd=ROOT, env=env, capture_output=True, text=True)
    if result.returncode != 0:
        sys.stderr.write(result.stderr[-4000:])
        return "cargo build failed"
    return None if IMAGE.is_file() else f"{IMAGE} was not written"


def judge(text: str, root: Path, commands: list[tuple[str, str]], listing_verb: str) -> list[str]:
    problems = []
    seen = dict(re.findall(r"^FTPFUSE:([a-z]+):(PASS|FAIL\S*)", text, re.M))
    for check in CHECKS:
        state = seen.get(check, "missing")
        print(f"  {check:8} {state}")
        if state != "PASS":
            problems.append(f"check {check}: {state}")
    if "FTPFUSE:UP /mnt/ftp" not in text:
        problems.append("ftpfuse never reported its mount")
    digest = hashlib.md5(BIG).hexdigest()
    # Not anchored: the console may interleave a stray byte before it.
    if not re.search(rf"{digest}\s+-", text):
        problems.append(f"the guest's md5 of big.bin is not {digest}")
    # The server's own files are the verdict.
    expect = {
        "hello.txt": b"HEllo from the host\nappended\n",
        "two words.txt": b"spaced\n",
        "big.bin": BIG,
        "newdir/up.txt": UPLOAD,
        "old.txt": b"new\n",
        "big2.bin": BIG,
    }
    for name, want in expect.items():
        path = root / name
        got = path.read_bytes() if path.is_file() else None
        ok = got == want
        print(f"  server {name:14} {'ok' if ok else 'DIFFERS'}")
        if not ok:
            problems.append(f"server file {name}: {len(got) if got is not None else 'missing'} bytes, "
                            f"wanted {len(want)}")
    leftovers = sorted(p.relative_to(root).as_posix() for p in root.rglob("*"))
    if leftovers != sorted(["big.bin", "big2.bin", "hello.txt", "newdir", "newdir/up.txt", "old.txt",
                            "two words.txt"]):
        problems.append(f"server tree: {leftovers}")
    big_retrs = commands.count(("RETR", "/big.bin"))
    print(f"  server sent big.bin {big_retrs} time(s)")
    if big_retrs > MAX_BIG_RETRS:
        problems.append(f"big.bin was fetched {big_retrs} times (read cache thrashing)")
    verbs = [verb for verb, _ in commands]
    for verb in (listing_verb, "RETR", "STOR", "APPE", "MKD", "RNFR", "RNTO", "DELE", "RMD"):
        if verb not in verbs:
            problems.append(f"the server never saw {verb}")
    return problems


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--no-build", action="store_true", help="reuse target/lazyos.img")
    parser.add_argument("--list", action="store_true",
                        help="the server refuses MLSD and RNTO overwrites: judge the LIST fallback and the "
                             "delete-then-rename path")
    parser.add_argument("--verbose", action="store_true", help="ftpfuse -v: log every FTP command and reply")
    parser.add_argument("--accel", default="auto", choices=["auto", "none", "tcg", "whpx", "kvm"])
    parser.add_argument("--qemu", help="path to qemu-system-x86_64")
    parser.add_argument("--timeout", type=float, default=900.0, help="seconds for the session")
    args = parser.parse_args()

    if not args.no_build:
        error = build()
        if error:
            return fail(error)
    OUT.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory() as served:
        root = Path(served)
        seed(root)
        server = ftpserver.FtpServer(root, user=USER, password=PASSWORD, mlsd=not args.list,
                                     overwrite=not args.list)
        script = OUT / "session.json"
        script.write_text(json.dumps(session(server.port, args.verbose), indent=1))
        command = [PY, str(SESSION), "--image", str(IMAGE), "--out", str(OUT), "--timeout", str(args.timeout),
                   "--script", str(script), "--accel", args.accel, "--net"]
        for pattern in FAIL_ON:
            command += ["--fail-on", pattern]
        if args.qemu:
            command += ["--qemu", args.qemu]
        print(f"ftpfuse: server on 127.0.0.1:{server.port}, session {script}", flush=True)
        try:
            code = subprocess.call(command, cwd=ROOT)
        finally:
            server.close()
        log = OUT / "serial.log"
        text = log.read_text(errors="replace") if log.is_file() else ""
        with server._lock:
            commands = list(server.commands)
        problems = judge(text, root, commands, "LIST" if args.list else "MLSD")
    if code != 0:
        problems.append(f"the session exited with {code}")
    if problems:
        for problem in problems:
            print(f"ftpfuse: {problem}", file=sys.stderr)
        return fail(f"{len(problems)} problem(s); see {OUT}/serial.log")
    print("FTPFUSE:HARNESS:PASS")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
