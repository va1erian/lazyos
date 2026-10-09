#!/usr/bin/env python3
"""`libs/smbwire` against real Samba (docs/smb-plan.md §9, "Harness peer"):
the harness server is ours, so the client is also judged against the server
`chatonnas` actually runs.

A pinned Alpine container (`apk add samba`) serves one share to one user with
a random password on a free host port, and `smbcat` (the library's host
client, `libs/smbwire/examples/smbcat.rs`) runs its round trip of every
operation with signing off, on request and required. Then Samba is
reconfigured twice: mandatory signing (the round trip still passes, and
`--no-sign` is refused before any file is touched) and required encryption
(the SMB 2.1 logon is refused with ACCESS_DENIED).

    python tools/smb/samba_interop.py          # needs Docker
    python tools/smb/samba_interop.py --keep   # leave the container running

Prints `SMB:INTEROP:PASS` or `SMB:INTEROP:FAIL <why>`.
"""

from __future__ import annotations

import argparse
import os
import secrets
import socket
import string
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent
IMAGE = "alpine:3.20"
NAME = "lazyos-smb-interop"
SMBCAT = ROOT / "target" / "debug" / "examples" / ("smbcat.exe" if os.name == "nt" else "smbcat")
CONF = """[global]
   workgroup = LAZYNAS
   server role = standalone server
   map to guest = never
   server min protocol = SMB2_02
[share]
   path = /srv/share
   read only = no
   valid users = chaton
"""
SETUP = """set -e
apk add --no-cache samba >/dev/null
adduser -D -H chaton
mkdir -p /srv/share && chown chaton /srv/share
printf '%s\\n' "$CONF" > /etc/samba/smb.conf
printf '%s\\n%s\\n' "$PW" "$PW" | smbpasswd -a -s chaton >/dev/null
echo SAMBA:READY
exec smbd -F --debug-stdout --no-process-group
"""


class DockerError(RuntimeError):
    """A `docker` command failed; the message carries its stderr."""


def docker(*args: str, check: bool = True, **kw) -> subprocess.CompletedProcess:
    result = subprocess.run(["docker", *args], capture_output=True, text=True, **kw)
    if check and result.returncode != 0:
        # CalledProcessError hides stderr, which is the only place the daemon says why.
        raise DockerError(f"docker {args[0]} exited {result.returncode}: "
                          f"{result.stderr.strip() or result.stdout.strip()}")
    return result


def pull_image(attempts: int = 4) -> None:
    """Pull `IMAGE`, retrying: a registry hiccup or rate limit is not a Samba failure."""
    for attempt in range(1, attempts + 1):
        try:
            docker("pull", "-q", IMAGE)
            return
        except DockerError as err:
            print(f"docker pull attempt {attempt}/{attempts}: {err}", file=sys.stderr)
            if attempt == attempts:
                raise
            time.sleep(5 * attempt)


def free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def wait_ready(port: int, timeout: float = 180.0) -> bool:
    deadline = time.time() + timeout
    while time.time() < deadline:
        logs = docker("logs", NAME, check=False).stdout
        if "SAMBA:READY" in logs and "smbd version" in logs:
            try:
                with socket.create_connection(("127.0.0.1", port), timeout=2):
                    return True
            except OSError:
                pass
        time.sleep(1)
    return False


def reconfigure(line: str) -> None:
    """Add `line` to `[global]` and make smbd reload it."""
    script = f"sed -i 's/^   map to guest = never/   map to guest = never\\n   {line}/' /etc/samba/smb.conf" \
             " && smbcontrol all reload-config"
    docker("exec", NAME, "sh", "-c", script)
    time.sleep(2)


def smbcat(port: int, password: str, *args: str, flags: tuple[str, ...] = ()) -> subprocess.CompletedProcess:
    env = dict(os.environ, LAZYOS_SMB_PASSWORD=password)
    return subprocess.run([str(SMBCAT), *flags, f"127.0.0.1:{port}", "share", "chaton", *args],
                          env=env, capture_output=True, text=True, timeout=120)


def run_checks(port: int, password: str) -> list[str]:
    problems = []

    def expect(label: str, result: subprocess.CompletedProcess, ok: bool, fragment: str = "") -> None:
        good = (result.returncode == 0) == ok and fragment in (result.stdout + result.stderr)
        print(f"  {label:34} {'ok' if good else 'WRONG'}")
        if not good:
            problems.append(f"{label}: exit {result.returncode}: {result.stderr.strip()[-300:]}")

    for flags in ((), ("--sign",), ("--sign-required",)):
        expect(f"round trip {' '.join(flags) or '(auto)'}", smbcat(port, password, "selftest", flags=flags), True,
               "selftest: ok")
    expect("wrong password", smbcat(port, password + "x", "ls"), False, "3221225581")
    expect("unknown share", subprocess.run(
        [str(SMBCAT), f"127.0.0.1:{port}", "nope", "chaton", "ls"], capture_output=True, text=True,
        env=dict(os.environ, LAZYOS_SMB_PASSWORD=password), timeout=60), False, "3221225676")
    reconfigure("server signing = mandatory")
    expect("mandatory signing, auto", smbcat(port, password, "selftest"), True, "signing: true")
    expect("mandatory signing, --no-sign", smbcat(port, password, "ls", flags=("--no-sign",)), False,
           "requires signing")
    reconfigure("server smb encrypt = required")
    expect("encryption required", smbcat(port, password, "ls"), False, "3221225506")
    return problems


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--keep", action="store_true", help="leave the container running")
    args = parser.parse_args()
    if subprocess.run(["docker", "version"], capture_output=True).returncode != 0:
        print("SMB:INTEROP:FAIL no running Docker engine")
        return 1
    subprocess.run(["cargo", "build", "-q", "-p", "smbwire", "--example", "smbcat"], cwd=ROOT, check=True)
    password = "".join(secrets.choice(string.ascii_letters + string.digits) for _ in range(20))
    port = free_port()
    try:
        pull_image()
    except DockerError as err:
        print(f"SMB:INTEROP:FAIL cannot pull {IMAGE}: {err}")
        return 1
    docker("rm", "-f", NAME, check=False)
    try:
        docker("run", "-d", "--name", NAME, "-p", f"127.0.0.1:{port}:445", "-e", f"CONF={CONF}",
               "-e", f"PW={password}", IMAGE, "sh", "-c", SETUP)
    except DockerError as err:
        print(f"SMB:INTEROP:FAIL cannot start the container: {err}")
        return 1
    try:
        if not wait_ready(port):
            print(docker("logs", NAME, check=False).stdout[-2000:])
            print("SMB:INTEROP:FAIL Samba did not start")
            return 1
        version = docker("exec", NAME, "smbd", "--version").stdout.strip()
        print(f"samba: {version} on 127.0.0.1:{port}")
        problems = run_checks(port, password)
    finally:
        if not args.keep:
            docker("rm", "-f", NAME, check=False)
    if problems:
        for problem in problems:
            print(f"smb interop: {problem}", file=sys.stderr)
        print(f"SMB:INTEROP:FAIL {len(problems)} problem(s)")
        return 1
    print("SMB:INTEROP:PASS")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
