"""Shared helpers of the Q0 spike tools (docs/netboot-plan.md, phase Q0).

Plain-name imports, like `tools/boot/`: every spike script and test sits in
this directory, so running one from anywhere puts the directory on
`sys.path`. This module adds `tools/boot` (the FAT reader in `preflight.py`)
and `tools/screenshot` (`qemu_qmp.py`) to the path once.
"""

from __future__ import annotations

import hashlib
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent.parent
for _dir in (ROOT / "tools" / "boot", ROOT / "tools" / "screenshot"):
    if str(_dir) not in sys.path:
        sys.path.insert(0, str(_dir))

#: Where the spike keeps everything it generates (git-ignored with `target/`).
SPIKE_DIR = ROOT / "target" / "netboot"
DEFAULT_PAYLOAD = SPIKE_DIR / "payload"
DEFAULT_IPXE = SPIKE_DIR / "ipxe"

#: The loader's file names, in the order the plan fetches them (the loader
#: reads kernel, then boot.json, then ramdisk; `bootx64.efi` is chained last).
PAYLOAD_FILES = ("kernel-x86_64", "boot.json", "ramdisk")
LOADER_NAME = "bootx64.efi"
#: The stick's path of the loader, inside the FAT.
LOADER_FAT_PATH = "EFI/BOOT/BOOTX64.EFI"

#: slirp's address for the host's loopback, as the guest sees it.
SLIRP_HOST = "10.0.2.2"
DEFAULT_BIND = "127.0.0.1:8420"

CHUNK = 1 << 20


def sha256_file(path: Path) -> str:
    """Hex SHA-256 of a file, read in chunks."""
    digest = hashlib.sha256()
    with open(path, "rb") as f:
        while chunk := f.read(CHUNK):
            digest.update(chunk)
    return digest.hexdigest()


def mib(n: int) -> str:
    """`n` bytes as MiB with one decimal."""
    return f"{n / (1 << 20):.1f} MiB"


def split_bind(bind: str) -> tuple[str, int]:
    """`HOST:PORT` to `(host, port)`; a bare port means the loopback, port 0 a free one."""
    host, sep, port = bind.rpartition(":")
    if not sep:
        host, port = "127.0.0.1", bind
    if not port.isdigit() or not 0 <= int(port) < 65536:
        raise ValueError(f"bad bind address {bind!r}: use HOST:PORT")
    return host or "127.0.0.1", int(port)
