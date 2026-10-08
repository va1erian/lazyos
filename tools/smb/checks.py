#!/usr/bin/env python3
"""What `tools/smb/run.py` does in the guest and what it expects back: the
harness servers (one per behaviour), the checks typed at the console, and
the session script that types them.

Each check is one `smb` invocation against one server. Its password is typed
at `smb`'s prompt with a `type_secret` step, so it is never in the script, the
serial log or the summary; afterwards the shell echoes `SMBCHECK:<name>:<status>`
so the judge can split the log by check.
"""

from __future__ import annotations

import sys
from dataclasses import dataclass, field
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "net"))
import hostpeers  # noqa: E402

#: The environment variables the session types from.
PASSWORD_VAR = "LAZYOS_SMB_PASSWORD"
WRONG_VAR = "LAZYOS_SMB_WRONG_PASSWORD"

UPLOAD_BYTES = 300_000
UPLOAD = hostpeers.xorshift_pattern(UPLOAD_BYTES)
SIGNED_UPLOAD = hostpeers.xorshift_pattern(100_000)
#: What the guest's `seq 1 20000` writes.
SEQ = "".join(f"{n}\n" for n in range(1, 20001)).encode()
BIG = bytes((i * 131 + (i >> 9)) & 0xFF for i in range(200_000))
HELLO = b"hello from the smb harness\n"


def seed(root: Path) -> None:
    """The share every server starts with."""
    (root / "hello.txt").write_bytes(HELLO)
    (root / "big.bin").write_bytes(BIG)
    (root / "old.txt").write_bytes(b"remove me\n")
    (root / "emptydir").mkdir()
    (root / "docs").mkdir()
    (root / "docs" / "readme.md").write_bytes(b"# read me\n")


@dataclass
class Server:
    """A harness server: its options (`smbserver.Options` fields)."""
    name: str
    options: dict = field(default_factory=dict)


SERVERS = [
    Server("main"),
    Server("signed", {"require_signing": True}),
    Server("sign", {}),
    Server("raw", {"spnego": False, "timestamp": False}),
    Server("guest", {"guest": True}),
    Server("encrypt", {"encrypt": True}),
    Server("truncated", {"truncate_challenge": True}),
    Server("tamper", {"require_signing": True, "tamper_read": True}),
    Server("smb3", {"dialects": (0x0311,)}),
]


@dataclass
class Check:
    name: str
    server: str
    commands: str
    passes: bool
    #: For a failure, a fragment the `SMB:FAIL reason=` line must contain.
    reason: str = ""
    flags: str = ""
    share: str = "share"
    secret: str = PASSWORD_VAR


CHECKS = [
    Check("transfer", "main",
          "ls ; get hello.txt - ; get big.bin ! ; put -g 300000 up.bin ; mkdir newdir ; "
          "mv up.bin newdir/up.bin ; ls newdir ; put /tmp/seq.txt seq.txt ; rm old.txt ; "
          "rmdir emptydir ; cd docs ; get readme.md ! ; df", True),
    Check("signed", "signed", "get big.bin ! ; put -g 100000 signed.bin", True),
    Check("sign", "sign", "get hello.txt !", True, flags="--sign"),
    Check("raw", "raw", "ls ; get hello.txt !", True),
    Check("wrongpw", "main", "ls", False, "LOGON_FAILURE", secret=WRONG_VAR),
    Check("noshare", "main", "ls", False, "BAD_NETWORK_NAME", share="nope"),
    Check("nosign", "signed", "ls", False, "requires signing", flags="--no-sign"),
    Check("guest", "guest", "ls", False, "guest"),
    Check("encrypt", "encrypt", "ls", False, "encryption"),
    Check("truncated", "truncated", "ls", False, "malformed"),
    Check("tamper", "tamper", "get hello.txt !", False, "signature"),
    Check("smb3", "smb3", "ls", False, "NEGOTIATE"),
]


def session(ports: dict[str, int], user: str) -> list[dict]:
    """The guest's steps."""
    steps: list[dict] = [{"wait_for": "/ #", "timeout": 300},
                         {"type": "seq 1 20000 > /tmp/seq.txt; echo SMBCHECK:ready"},
                         {"key": "enter", "until": "SMBCHECK:ready", "timeout": 60, "retries": 1}]
    for check in CHECKS:
        flags = f"{check.flags} " if check.flags else ""
        line = (f"smb {flags}-p {ports[check.server]} -U {user} //10.0.2.2/{check.share} "
                f"'{check.commands}'; echo SMBCHECK:{check.name}:$?")
        # The commands are one quoted word: `;` is the shell's separator too,
        # and `smb` joins its arguments back into one line. Wait for the
        # prompt, not a fixed time: on a cold boot `netd` may still
        # be getting its address.
        steps += [{"type": line},
                  {"key": "enter", "until": "Password for", "timeout": 120, "retries": 1},
                  {"type_secret": check.secret},
                  {"key": "enter", "until": f"SMBCHECK:{check.name}:", "timeout": 300}]
    steps += [{"shot": "smb"}, {"quit": True}]
    return steps
