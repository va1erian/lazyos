#!/usr/bin/env python3
"""SMB stage F3 end to end (docs/smb-plan.md §9): a share mounted with
`smbfuse` in a booted LazyOS and used as a directory by ordinary programs,
judged from what the servers hold afterwards and what crossed the wire.

1. Two harness servers (`smbserver.py`): a plain one and one requiring
   signing, each serving a seeded temporary directory; the guest reaches
   them at 10.0.2.2.
2. `cargo build` with `LAZYOS_CLI=1 LAZYOS_NETD=1` (`smbfuse` is in every
   networking image).
3. A headless session (`fuse_checks.py`): `smbfuse` mounts the share at
   `/mnt/share` (the password typed at its prompt with `type_secret`), then
   BusyBox `ls`, `cat`, `md5sum`, `cp` both ways, `cmp`, `>>`, an in-place
   `dd`, `mkdir`, `mv`, `rm`, `rmdir`, `df` and `sync` run through it; a
   second mount requires signing; a wrong password and an unknown share are
   refused with their reasons and leave nothing under `/mnt`.
4. The verdict: every step's status and output; each server's directory is
   exactly what the steps made; the signing server verified every request;
   the capture shows dialect 2.1, signatures where required, the upload
   rebuilt from WRITE data alone and the download from READ data alone; and
   no password appears in the capture, the serial log or the session.

    python tools/smb/fuse_run.py              # build, boot, judge
    python tools/smb/fuse_run.py --no-build   # reuse target/lazyos.img

`LAZYOS_SMB_USER` and `LAZYOS_SMB_PASSWORD` set the account (letters and
digits); without them a random password is made for the run. Output goes to
`shots/smbfuse/`.
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(ROOT / "tools" / "net"))
sys.path.insert(0, str(ROOT / "tools" / "abi"))
import busybox  # noqa: E402
import checks  # noqa: E402
import fuse_checks as f3  # noqa: E402
import judge  # noqa: E402
import pcap  # noqa: E402
import smb_pcap  # noqa: E402
import smbserver  # noqa: E402

PY = sys.executable
SESSION = ROOT / "tools" / "screenshot" / "qemu_session.py"
IMAGE = ROOT / "target" / "lazyos.img"
OUT = ROOT / "shots" / "smbfuse"
GUEST_IP = pcap.parse_ip("10.0.2.15")
GATEWAY_IP = pcap.parse_ip("10.0.2.2")
FAIL_ON = (r"user: task [0-9]+ killed by", r"panicked", "PANIC", "EXCEPTION")
ARTIFACTS = ("serial.log", "net.pcap", "summary.json", "session.json")


def fail(message: str) -> int:
    print(f"SMBFUSE:HARNESS:FAIL: {message}", file=sys.stderr)
    return 1


def build() -> str | None:
    shell = os.environ.get("LAZYOS_BUSYBOX")
    if not (shell and Path(shell).is_file()):
        found = busybox.ensure_busybox()
        if found is None:
            return "no BusyBox: run `python tools/abi/busybox.py`, or set LAZYOS_BUSYBOX"
        shell = str(found)
    env = dict(os.environ, LAZYOS_BUSYBOX=shell, LAZYOS_CLI="1", LAZYOS_NETD="1", LAZYOS_NETD_ARGS="demo=0",
               LAZYOS_RESET_OS="1")
    env.pop("LAZYOS_DESKTOP", None)
    print("smbfuse: LAZYOS_CLI=1 LAZYOS_NETD=1 cargo build", flush=True)
    result = subprocess.run(["cargo", "build"], cwd=ROOT, env=env, capture_output=True, text=True)
    if result.returncode != 0:
        sys.stderr.write(result.stderr[-4000:])
        return "cargo build failed"
    return None if IMAGE.is_file() else f"{IMAGE} was not written"


def judge_records(records: dict[str, smbserver.Record]) -> list[str]:
    problems = []
    signed = records["signed"]
    if signed.signed == 0 or signed.unsigned or signed.bad_signatures:
        problems.append(f"signed server: signed={signed.signed} unsigned={signed.unsigned} "
                        f"bad={signed.bad_signatures}")
    main = records["main"]
    if main.signed:
        problems.append(f"main server: {main.signed} signed requests though nobody asked for signing")
    if not any(not ok for _, _, ok in main.logons):
        problems.append("main server: the wrong password never reached it")
    if not any(command == "FLUSH" for command, _ in main.events):
        problems.append("main server: `sync` sent no FLUSH")
    return problems


def wire_expectations(ports: dict[str, int]) -> dict[int, smb_pcap.PortExpect]:
    E = smb_pcap.PortExpect
    return {
        # The mount, then the wrong password and the unknown share.
        ports["main"]: E(signed=False, uploads=[f3.BIG_TXT], downloads=[checks.BIG], min_flows=3),
        ports["signed"]: E(signed=True, uploads=[checks.SEQ], downloads=[checks.BIG]),
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--no-build", action="store_true", help="reuse target/lazyos.img")
    parser.add_argument("--accel", default="auto", choices=["auto", "none", "tcg", "whpx", "kvm"])
    parser.add_argument("--qemu", help="path to qemu-system-x86_64")
    parser.add_argument("--timeout", type=float, default=1200.0, help="seconds for the session")
    args = parser.parse_args()

    user, password, wrong = smbserver_credentials()
    if not args.no_build:
        error = build()
        if error:
            return fail(error)
    OUT.mkdir(parents=True, exist_ok=True)
    for stale in ARTIFACTS:
        (OUT / stale).unlink(missing_ok=True)
    with tempfile.TemporaryDirectory() as served:
        roots, servers = {}, {}
        for spec in f3.SERVERS:
            roots[spec.name] = Path(served) / spec.name
            roots[spec.name].mkdir()
            checks.seed(roots[spec.name])
            options = smbserver.Options(user=user, password=password, **spec.options)
            servers[spec.name] = smbserver.SmbServer(roots[spec.name], 0, options)
        ports = {name: server.port for name, server in servers.items()}
        script = OUT / "session.json"
        script.write_text(json.dumps(f3.session(ports, user), indent=1))
        command = [PY, str(SESSION), "--image", str(IMAGE), "--out", str(OUT), "--timeout", str(args.timeout),
                   "--script", str(script), "--accel", args.accel, "--net", "--net-forward", "none",
                   "--net-pcap", str(OUT / "net.pcap")]
        for pattern in FAIL_ON:
            command += ["--fail-on", pattern]
        if args.qemu:
            command += ["--qemu", args.qemu]
        print(f"smbfuse: servers {ports}, session {script}", flush=True)
        env = dict(os.environ, **{f3.PASSWORD_VAR: password, f3.WRONG_VAR: wrong})
        try:
            code = subprocess.call(command, cwd=ROOT, env=env)
        finally:
            for server in servers.values():
                server.close()
        log = OUT / "serial.log"
        text = log.read_text(errors="replace") if log.is_file() else ""
        problems = f3.judge_steps(text)
        problems += f3.judge_tree(roots["main"], f3.MAIN_FILES, f3.MAIN_DIRS)
        problems += f3.judge_tree(roots["signed"], f3.SIGNED_FILES, f3.SIGNED_DIRS)
        problems += judge_records({n: s.record for n, s in servers.items()})
    secret_bytes = [password.encode(), wrong.encode()]
    frames = pcap.read_pcap(OUT / "net.pcap") if (OUT / "net.pcap").is_file() else []
    flows, wire = smb_pcap.check_smb_flows(frames, GUEST_IP, GATEWAY_IP, wire_expectations(ports),
                                           secret_bytes + [s.encode("utf-16-le") for s in (password, wrong)])
    print(f"  wire: {flows} SMB connection(s), {len(wire)} problem(s)")
    problems += wire
    problems += judge.leaks([OUT / n for n in ARTIFACTS], secret_bytes)
    if code != 0:
        problems.append(f"the session exited with {code}")
    if problems:
        for problem in problems:
            print(f"smbfuse: {problem}", file=sys.stderr)
        return fail(f"{len(problems)} problem(s); see {OUT}/serial.log")
    print("SMBFUSE:HARNESS:PASS")
    return 0


def smbserver_credentials() -> tuple[str, str, str]:
    """`run.py`'s rule: the account from the environment, else a random one."""
    import secrets
    import string

    alphabet = string.ascii_letters + string.digits
    user = os.environ.get("LAZYOS_SMB_USER", "chaton")
    password = os.environ.get("LAZYOS_SMB_PASSWORD") or "".join(secrets.choice(alphabet) for _ in range(16))
    if not password.isalnum() or not user.isalnum():
        raise SystemExit("LAZYOS_SMB_USER and LAZYOS_SMB_PASSWORD must be letters and digits (they are typed)")
    return user, password, password[::-1] + "x"


if __name__ == "__main__":
    raise SystemExit(main())
