"""USB stick images for the storage harness: an MBR disk with one Linux
partition holding an ext2 volume made by `tools/mkdisk`, and the host-side
checks run on it afterwards (`e2fsck -fn`, `debugfs`).

    python tools/storage/stick.py target/stick.img            # a lazyhome stick
    python tools/storage/stick.py other.img --label otherdisk # any other label
"""

from __future__ import annotations

import argparse
import shutil
import struct
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent
SECTOR = 512
#: The partition starts 1 MiB in, as every partitioning tool does.
START_LBA = 2048
#: Linux native partition type.
LINUX = 0x83


def mbr(start: int, sectors: int) -> bytes:
    """A boot sector with one partition entry (CHS fields unused, 0xFE)."""
    entry = struct.pack("<B3sB3sII", 0x00, b"\xfe\xff\xff", LINUX, b"\xfe\xff\xff",
                        start, sectors)
    sector = bytearray(SECTOR)
    sector[446:462] = entry
    sector[510:512] = b"\x55\xaa"
    return bytes(sector)


def make_volume(path: Path, size: str, label: str) -> None:
    """A home volume (`<user>/` at its root, owned by the account) via mkdisk."""
    command = [sys.executable, "-m", "tools.mkdisk", str(path), "--home-volume",
               "--size", size, "--label", label, "--force"]
    subprocess.run(command, cwd=ROOT, check=True, stdout=subprocess.DEVNULL)


def make_stick(path: Path, size: str = "32M", label: str = "lazyhome") -> None:
    """Write `path`: MBR, then the volume at [`START_LBA`]."""
    with tempfile.TemporaryDirectory() as scratch:
        volume = Path(scratch) / "volume.img"
        make_volume(volume, size, label)
        data = volume.read_bytes()
    if len(data) % SECTOR:
        raise SystemExit("volume is not whole sectors")
    head = mbr(START_LBA, len(data) // SECTOR) + bytes((START_LBA - 1) * SECTOR)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(head + data)


def partition(path: Path) -> bytes:
    """The bytes of the stick's partition, as its MBR describes them."""
    data = path.read_bytes()
    start, sectors = struct.unpack_from("<II", data, 446 + 8)
    return data[start * SECTOR:(start + sectors) * SECTOR]


def fsck(path: Path) -> tuple[bool, str]:
    """`e2fsck -fn` on the stick's partition: (clean, output)."""
    if not shutil.which("e2fsck"):
        return False, "e2fsck not installed"
    with tempfile.TemporaryDirectory() as scratch:
        volume = Path(scratch) / "volume.img"
        volume.write_bytes(partition(path))
        result = subprocess.run(["e2fsck", "-fn", str(volume)], capture_output=True, text=True)
    return result.returncode == 0, result.stdout + result.stderr


def read_file(path: Path, name: str) -> str | None:
    """A file on the stick's partition (`debugfs`), or None."""
    if not shutil.which("debugfs"):
        return None
    with tempfile.TemporaryDirectory() as scratch:
        volume = Path(scratch) / "volume.img"
        volume.write_bytes(partition(path))
        result = subprocess.run(["debugfs", "-R", f"cat {name}", str(volume)],
                                capture_output=True, text=True, errors="replace")
    return result.stdout if result.returncode == 0 else None


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("path", type=Path)
    parser.add_argument("--size", default="32M")
    parser.add_argument("--label", default="lazyhome")
    args = parser.parse_args()
    make_stick(args.path, args.size, args.label)
    clean, output = fsck(args.path)
    print(output.strip())
    return 0 if clean else 1


if __name__ == "__main__":
    sys.exit(main())
