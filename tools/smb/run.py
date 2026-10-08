#!/usr/bin/env python3
"""SMB stage F2 end to end (docs/smb-plan.md §9): the `smb` client in a booted
LazyOS against SMB 2.1 servers on the host, judged from what the servers
recorded and what crossed the wire.

1. Nine harness servers (`smbserver.py`, standard library only) serve seeded
   temporary directories on the host loopback, one per behaviour: plain,
   signing required, raw NTLM, and the misbehaving ones (guest logon,
   encryption required, a truncated challenge, a tampered signature, SMB3
   only). The guest reaches them at 10.0.2.2.
2. `cargo build` with `LAZYOS_CLI=1 LAZYOS_NETD=1 LAZYOS_SMB=1`.
3. A headless session runs one `smb` per check (`checks.py`), typing each
   password at `smb`'s prompt from the host environment (`type_secret`), and
   captures the network (`--net-pcap`).
4. The verdict (`judge.py`, `smb_pcap.py`): every check passed or was refused
   for its reason; the main server's directory holds exactly the uploaded
   bytes; the signing servers verified every request's signature; refused
   sessions never reached a file; the capture shows dialect 2.1, the share,
   signatures where required, uploads only inside WRITEs and downloads only
   inside READs; and no password appears in the capture, the serial log or the
   session record.

    python tools/smb/run.py                # build, boot, judge
    python tools/smb/run.py --no-build     # reuse target/lazyos.img

`LAZYOS_SMB_USER` and `LAZYOS_SMB_PASSWORD` set the account (letters and
digits); without them a random password is made for the run. Nothing is
committed. Output goes to `shots/smb/`.
"""

from __future__ import annotations

import argparse
import json
import os
import secrets
import string
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
import judge  # noqa: E402
import pcap  # noqa: E402
import smb_pcap  # noqa: E402
import smbserver  # noqa: E402

PY = sys.executable
SESSION = ROOT / "tools" / "screenshot" / "qemu_session.py"
IMAGE = ROOT / "target" / "lazyos.img"
OUT = ROOT / "shots" / "smb"
GUEST_IP = pcap.parse_ip("10.0.2.15")
GATEWAY_IP = pcap.parse_ip("10.0.2.2")
FAIL_ON = (r"user: task [0-9]+ killed by", r"panicked", "PANIC", "EXCEPTION")


def fail(message: str) -> int:
    print(f"SMB:HARNESS:FAIL: {message}", file=sys.stderr)
    return 1


def credentials() -> tuple[str, str, str]:
    """(user, password, a wrong password), all typeable."""
    alphabet = string.ascii_letters + string.digits
    user = os.environ.get("LAZYOS_SMB_USER", "chaton")
    password = os.environ.get("LAZYOS_SMB_PASSWORD") or "".join(secrets.choice(alphabet) for _ in range(16))
    if not password.isalnum() or not user.isalnum():
        raise SystemExit("LAZYOS_SMB_USER and LAZYOS_SMB_PASSWORD must be letters and digits (they are typed)")
    return user, password, password[::-1] + "x"


def build() -> str | None:
    shell = os.environ.get("LAZYOS_BUSYBOX")
    if not (shell and Path(shell).is_file()):
        found = busybox.ensure_busybox()
        if found is None:
            return "no BusyBox: run `python tools/abi/busybox.py`, or set LAZYOS_BUSYBOX"
        shell = str(found)
    env = dict(os.environ, LAZYOS_BUSYBOX=shell, LAZYOS_CLI="1", LAZYOS_NETD="1", LAZYOS_NETD_ARGS="demo=0",
               LAZYOS_SMB="1", LAZYOS_RESET_OS="1")
    env.pop("LAZYOS_DESKTOP", None)
    print("smb: LAZYOS_CLI=1 LAZYOS_NETD=1 LAZYOS_SMB=1 cargo build", flush=True)
    result = subprocess.run(["cargo", "build"], cwd=ROOT, env=env, capture_output=True, text=True)
    if result.returncode != 0:
        sys.stderr.write(result.stderr[-4000:])
        return "cargo build failed"
    return None if IMAGE.is_file() else f"{IMAGE} was not written"


def wire_expectations(ports: dict[str, int]) -> dict[int, smb_pcap.PortExpect]:
    E = smb_pcap.PortExpect
    return {
        # Two flows: the transfer and the two refused logons on the same server.
        ports["main"]: E(signed=False, uploads=[checks.UPLOAD, checks.SEQ], downloads=[checks.BIG], min_flows=3),
        ports["signed"]: E(signed=True, uploads=[checks.SIGNED_UPLOAD], downloads=[checks.BIG], min_flows=2),
        ports["sign"]: E(signed=True),
        ports["raw"]: E(signed=False),
        ports["guest"]: E(share=None),
        ports["encrypt"]: E(share=None),
        ports["truncated"]: E(share=None),
        ports["tamper"]: E(),
        ports["smb3"]: E(dialect=None, share=None),
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--no-build", action="store_true", help="reuse target/lazyos.img")
    parser.add_argument("--accel", default="auto", choices=["auto", "none", "tcg", "whpx", "kvm"])
    parser.add_argument("--qemu", help="path to qemu-system-x86_64")
    parser.add_argument("--timeout", type=float, default=1200.0, help="seconds for the session")
    args = parser.parse_args()

    user, password, wrong = credentials()
    if not args.no_build:
        error = build()
        if error:
            return fail(error)
    OUT.mkdir(parents=True, exist_ok=True)
    for stale in ("serial.log", "net.pcap", "summary.json", "session.json"):
        (OUT / stale).unlink(missing_ok=True)
    with tempfile.TemporaryDirectory() as served:
        roots, servers = {}, {}
        for spec in checks.SERVERS:
            roots[spec.name] = Path(served) / spec.name
            roots[spec.name].mkdir()
            checks.seed(roots[spec.name])
            options = smbserver.Options(user=user, password=password, **spec.options)
            servers[spec.name] = smbserver.SmbServer(roots[spec.name], 0, options)
        ports = {name: server.port for name, server in servers.items()}
        script = OUT / "session.json"
        script.write_text(json.dumps(checks.session(ports, user), indent=1))
        command = [PY, str(SESSION), "--image", str(IMAGE), "--out", str(OUT), "--timeout", str(args.timeout),
                   "--script", str(script), "--accel", args.accel, "--net", "--net-forward", "none",
                   "--net-pcap", str(OUT / "net.pcap")]
        for pattern in FAIL_ON:
            command += ["--fail-on", pattern]
        if args.qemu:
            command += ["--qemu", args.qemu]
        print(f"smb: servers {ports}, session {script}", flush=True)
        env = dict(os.environ, **{checks.PASSWORD_VAR: password, checks.WRONG_VAR: wrong})
        try:
            code = subprocess.call(command, cwd=ROOT, env=env)
        finally:
            for server in servers.values():
                server.close()
        log = OUT / "serial.log"
        text = log.read_text(errors="replace") if log.is_file() else ""
        problems = judge.judge_markers(text) + judge.judge_transfer(text)
        problems += judge.judge_main_tree(roots["main"])
        problems += judge.judge_records({n: s.record for n, s in servers.items()}, roots)
    secret_bytes = [password.encode(), wrong.encode()]
    frames = pcap.read_pcap(OUT / "net.pcap") if (OUT / "net.pcap").is_file() else []
    flows, wire = smb_pcap.check_smb_flows(frames, GUEST_IP, GATEWAY_IP, wire_expectations(ports),
                                           secret_bytes + [s.encode("utf-16-le") for s in (password, wrong)])
    print(f"  wire: {flows} SMB connection(s), {len(wire)} problem(s)")
    problems += wire
    problems += judge.leaks([OUT / n for n in ("serial.log", "net.pcap", "summary.json", "session.json")],
                            secret_bytes)
    if code != 0:
        problems.append(f"the session exited with {code}")
    if problems:
        for problem in problems:
            print(f"smb: {problem}", file=sys.stderr)
        return fail(f"{len(problems)} problem(s); see {OUT}/serial.log")
    print("SMB:HARNESS:PASS")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
