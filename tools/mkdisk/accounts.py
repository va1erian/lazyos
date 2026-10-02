"""The demo accounts, read from the one file that defines them.

``build_support/passwd`` is the single account file (issue #508): ``build.rs``
installs it byte for byte as the OS volume's ``/system/etc/passwd``, the only
account source ``accountsd`` reads (it has no built-in table and fails closed
without the file). The home-volume seed reads the same file here, so the homes
it creates always match the accounts the system boots with.
"""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent
PASSWD_FILE = ROOT / "build_support" / "passwd"
BUILD_SCRIPT = ROOT / "build.rs"
_FIELDS = 6  # name:uid:gid:secret:home:shell


@dataclass(frozen=True)
class Account:
    """One passwd line, without the (bring-up plaintext) secret."""

    name: str
    uid: int
    gid: int
    home: str
    shell: str


def parse_passwd(text: str) -> list[Account]:
    """Parse ``name:uid:gid:secret:home:shell`` lines.

    Blank lines and ``#`` comments are skipped, as ``accountsd`` skips them; any
    other malformed line is an error rather than a silently missing home.
    """
    accounts = []
    for line in text.splitlines():
        if not line.strip() or line.startswith("#"):
            continue
        fields = line.split(":")
        if len(fields) != _FIELDS:
            raise ValueError(f"malformed passwd line {line!r}")
        name, uid, gid, _secret, home, shell = fields
        if not (uid.isdigit() and gid.isdigit()):
            raise ValueError(f"malformed uid/gid in passwd line {line!r}")
        accounts.append(Account(name, int(uid), int(gid), home, shell))
    return accounts


def image_passwd(path: Path = PASSWD_FILE) -> str:
    """The raw ``/system/etc/passwd`` the build installs."""
    try:
        return path.read_bytes().decode("utf-8")
    except OSError as exc:
        raise ValueError(f"cannot read the account file {path}: {exc}") from exc


def demo_accounts(path: Path = PASSWD_FILE) -> list[Account]:
    """The accounts the demo boots with."""
    return parse_passwd(image_passwd(path))
