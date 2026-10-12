"""Key files and `keys.json` (docs/netboot-plan.md section 4, decision 4).

    key.sec   the 32-byte seed as 64 lower-case hex plus a newline (mode 0600)
    key.pub   the 32-byte public key, the same way
    keys.json {"keys": [{"id": "<keyid>", "pub": "<64 hex>", "name": "<text>"}]}

The key id is the first 16 hex characters of SHA-256 of the raw public key.
`manifest.sig` is 128 hex characters and a newline. The secret never goes to
the registry: only `keygen.py` and `push.py` read `key.sec`.
"""

from __future__ import annotations

import hashlib
import os
import socket
from pathlib import Path

import ed25519
import jsonio
import names


def key_id(pub: bytes) -> str:
    return hashlib.sha256(pub).hexdigest()[:16]


def _read_hex(path: Path, regex, what: str) -> bytes:
    try:
        text = Path(path).read_text(encoding="ascii").strip()
    except (OSError, UnicodeDecodeError) as e:
        raise ValueError(f"cannot read {what} {path}: {e}") from e
    if not regex.match(text):
        raise ValueError(f"{path} is not {what} (expected 64 lower-case hex characters)")
    return bytes.fromhex(text)


def read_secret(path: Path) -> bytes:
    """The seed in a `key.sec`."""
    return _read_hex(path, names.HEX64, "a secret key file")


def read_public(path: Path) -> bytes:
    """The public key in a `key.pub`."""
    return _read_hex(path, names.HEX64, "a public key file")


def write_keypair(directory: Path, seed: bytes) -> tuple[Path, Path]:
    """Write `key.sec` (0600 where the OS has modes) and `key.pub` into `directory`."""
    directory = Path(directory)
    directory.mkdir(parents=True, exist_ok=True)
    sec, pub = directory / "key.sec", directory / "key.pub"
    fd = os.open(sec, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(fd, "w", encoding="ascii", newline="\n") as f:
        f.write(seed.hex() + "\n")
    try:
        os.chmod(sec, 0o600)  # an existing file keeps its old mode otherwise
    except OSError:
        pass
    pub.write_text(ed25519.public_key(seed).hex() + "\n", encoding="ascii", newline="\n")
    return sec, pub


def keys_entry(pub: bytes, name: str | None = None) -> dict:
    """The `keys.json` entry for a public key (`push.py --register-key`)."""
    return {"id": key_id(pub), "pub": pub.hex(), "name": name or socket.gethostname()}


def load_keys(path: Path) -> dict[str, bytes]:
    """`{key id: public key}` from a `keys.json`; a missing file or a bad entry gives none.

    An entry whose `id` does not match its `pub` is skipped rather than
    trusted: the id is how a request names its key, so it must be the hash.
    """
    doc = jsonio.read_json(path, {})
    out: dict[str, bytes] = {}
    entries = doc.get("keys", []) if isinstance(doc, dict) else []
    for entry in entries if isinstance(entries, list) else []:
        pub_hex = entry.get("pub") if isinstance(entry, dict) else None
        if not isinstance(pub_hex, str) or not names.HEX64.match(pub_hex):
            continue
        pub = bytes.fromhex(pub_hex)
        if entry.get("id", key_id(pub)) == key_id(pub):
            out[key_id(pub)] = pub
    return out


def parse_signature(text: str | bytes) -> bytes | None:
    """The 64 bytes of a `manifest.sig` (128 lower-case hex, a trailing newline), else None."""
    if isinstance(text, bytes):
        try:
            text = text.decode("ascii")
        except UnicodeDecodeError:
            return None
    if text.endswith("\n"):  # the one trailing newline (a CRLF from a Windows editor too)
        text = text[:-1].removesuffix("\r")
    return bytes.fromhex(text) if names.HEX128.match(text) else None


def format_signature(sig: bytes) -> str:
    """A signature as `manifest.sig` holds it."""
    return sig.hex() + "\n"
