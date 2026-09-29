"""The demo accounts, read from the file that defines them.

``accountsd`` keeps its built-in passwd table as a Rust string constant, and
``build.rs`` embeds a second copy as the boot volume's ``PASSWD``. Python cannot
import either, and a hand-copied list here would silently go stale, so the seed
users are parsed out of ``accountsd.rs`` and ``test_mkdisk.py`` fails if
``build.rs`` disagrees with it.
"""

from __future__ import annotations

import re
from dataclasses import dataclass
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent
ACCOUNTSD_SOURCE = ROOT / "user" / "src" / "bin" / "accountsd.rs"
BUILD_SCRIPT = ROOT / "build.rs"

# `const BUILTIN: &str = "root:0:0:...\nalice:...\n";` (a one-line literal).
_BUILTIN = re.compile(r'const\s+BUILTIN\s*:\s*&str\s*=\s*"((?:[^"\\]|\\.)*)"\s*;')
# The same table as build.rs writes it: `b"root:...\n".to_vec()` for `PASSWD`.
_BUILD_PASSWD = re.compile(
    r'String::from\("PASSWD"\)\s*,\s*b"((?:[^"\\]|\\.)*)"\.to_vec\(\)', re.S)
_ESCAPES = {"n": "\n", "\\": "\\", '"': '"'}
_FIELDS = 6  # name:uid:gid:secret:home:shell


@dataclass(frozen=True)
class Account:
    """One passwd line, without the (bring-up plaintext) secret."""

    name: str
    uid: int
    gid: int
    home: str
    shell: str


def unescape_rust(literal: str) -> str:
    """Decode the few escapes a passwd table uses; refuse anything else.

    Guessing at ``\\x41`` or ``\\u{..}`` would let the seed diverge from what the
    daemon really parses, so an unknown escape is an error, not a pass-through.
    """
    out, chars = [], iter(literal)
    for char in chars:
        if char != "\\":
            out.append(char)
            continue
        escape = next(chars, "")
        if escape not in _ESCAPES:
            raise ValueError(f"unsupported escape \\{escape} in the passwd literal")
        out.append(_ESCAPES[escape])
    return "".join(out)


def parse_passwd(text: str) -> list[Account]:
    """Parse ``name:uid:gid:secret:home:shell`` lines (blank lines skipped)."""
    accounts = []
    for line in text.splitlines():
        if not line.strip():
            continue
        fields = line.split(":")
        if len(fields) != _FIELDS:
            raise ValueError(f"malformed passwd line {line!r}")
        name, uid, gid, _secret, home, shell = fields
        accounts.append(Account(name, int(uid), int(gid), home, shell))
    return accounts


def _extract(pattern: re.Pattern, path: Path, what: str) -> str:
    match = pattern.search(path.read_text(encoding="utf-8"))
    if not match:
        raise ValueError(f"cannot find {what} in {path}; update tools/mkdisk/accounts.py")
    return unescape_rust(match.group(1))


def builtin_passwd(source: Path = ACCOUNTSD_SOURCE) -> str:
    """The raw built-in passwd table from ``accountsd.rs``."""
    return _extract(_BUILTIN, source, "the BUILTIN passwd table")


def image_passwd(source: Path = BUILD_SCRIPT) -> str:
    """The raw ``PASSWD`` file ``build.rs`` puts on the boot volume."""
    return _extract(_BUILD_PASSWD, source, "the PASSWD file contents")


def demo_accounts(source: Path = ACCOUNTSD_SOURCE) -> list[Account]:
    """The accounts the demo boots with."""
    return parse_passwd(builtin_passwd(source))
