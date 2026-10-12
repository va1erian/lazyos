"""A minimal FAT16 formatter: an MBR disk with one FAT16 partition, in pure Python.

Enough for the spike's ESP-only image (`make_esp_image.py`): a few files in
a few directories, each laid out contiguously. Names that are not upper-case
8.3 get long-name entries (so `kernel-x86_64` and `ramdisk` keep their case
for the firmware), directories start with `.` and `..`, and the whole image
is deterministic (fixed dates and volume id). The reader that judges it in
the tests is `tools/boot/preflight.py`'s, the one that judges the stick.
"""

from __future__ import annotations

import shutil
import struct
from pathlib import Path

SECTOR = 512
ROOT_ENTRIES = 512
FAT16_MIN_CLUSTERS = 4085 + 11  # a few over the FAT12 limit, which readers go by
FAT16_MAX_CLUSTERS = 65524
SLACK_CLUSTERS = 32
DOS_DATE = ((2026 - 1980) << 9) | (1 << 5) | 1  # 2026-01-01, fixed
VOLUME_ID = 0x4C5A4F53
DISK_SIGNATURE = 0x4C5A5350
_VALID = set("ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789!#$%&'()-@^_`{}~")


class FormatError(Exception):
    """The files do not fit a FAT16 volume, or a name cannot be stored."""


def lfn_checksum(short: bytes) -> int:
    total = 0
    for c in short:
        total = (((total & 1) << 7) + (total >> 1) + c) & 0xFF
    return total


def _clean(part: str) -> str:
    return "".join(c for c in part.upper() if c in _VALID)


def short_name(name: str, taken: set[bytes]) -> tuple[bytes, bool]:
    """The 11-byte 8.3 name for `name` and whether long-name entries are needed.

    A name that already fits 8.3 is stored as its upper-case self (with
    long-name entries unless it was upper case already, to keep its case);
    any other gets `BASE~N`, N chosen so that it is unique in `taken`.
    """
    base, ext = name.rsplit(".", 1) if "." in name else (name, "")
    fits = (0 < len(base) <= 8 and len(ext) <= 3 and name.count(".") <= 1
            and all(c in _VALID for c in (base + ext).upper()))
    if fits:
        candidate = (base.upper().ljust(8) + ext.upper().ljust(3)).encode()
        if candidate not in taken:
            return candidate, name != name.upper()
    stem, ext = _clean(base)[:6] or "FILE", _clean(ext)[:3]
    for n in range(1, 1000000):
        tail = f"~{n}"
        candidate = (stem[: 8 - len(tail)] + tail).ljust(8) + ext.ljust(3)
        if candidate.encode() not in taken:
            return candidate.encode(), True
    raise FormatError(f"no short name for {name!r}")


def lfn_entries(name: str, short: bytes) -> list[bytes]:
    """The long-name directory entries for `name`, in on-disk order (last part first)."""
    units = list(name.encode("utf-16le"))
    chars = [units[i] | units[i + 1] << 8 for i in range(0, len(units), 2)]
    if len(chars) % 13:  # a name that fills its last entry needs no terminator
        chars.append(0)
        while len(chars) % 13:
            chars.append(0xFFFF)
    check = lfn_checksum(short)
    count = len(chars) // 13
    out = []
    for index in range(count, 0, -1):
        part = chars[(index - 1) * 13 : index * 13]
        seq = index | (0x40 if index == count else 0)
        raw = bytearray(32)
        raw[0] = seq
        raw[11], raw[12], raw[13] = 0x0F, 0, check
        raw[1:11] = struct.pack("<5H", *part[0:5])
        raw[14:26] = struct.pack("<6H", *part[5:11])
        raw[28:32] = struct.pack("<2H", *part[11:13])
        out.append(bytes(raw))
    return out


def dir_entry(short: bytes, attr: int, cluster: int, size: int) -> bytes:
    raw = bytearray(32)
    raw[0:11] = short
    raw[11] = attr
    struct.pack_into("<HHHHHHHI", raw, 14, 0, DOS_DATE, DOS_DATE, 0, 0, DOS_DATE, cluster, size)
    return bytes(raw)


class Node:
    """A directory: files (name -> source path) and subdirectories."""

    def __init__(self) -> None:
        self.files: dict[str, Path] = {}
        self.dirs: dict[str, "Node"] = {}
        self.cluster = 0
        self.layout: list[tuple[str, bytes, bool, object]] = []  # name, short, is_dir, payload

    def add(self, path: str, source: Path) -> None:
        head, _, rest = path.partition("/")
        if not rest:
            if head in self.files or head in self.dirs:
                raise FormatError(f"{path} is added twice")
            self.files[head] = source
        else:
            self.dirs.setdefault(head, Node()).add(rest, source)

    def prepare(self, is_root: bool) -> int:
        """Pick short names; the number of 32-byte entries this directory needs."""
        taken: set[bytes] = set()
        self.layout = []
        entries = 0 if is_root else 2
        for name in sorted(self.dirs) + sorted(self.files):
            short, long_needed = short_name(name, taken)
            taken.add(short)
            entries += 1 + (len(lfn_entries(name, short)) if long_needed else 0)
            self.layout.append((name, short, name in self.dirs, long_needed))
        if is_root:
            entries += 1  # the volume label
        return entries

    def walk(self, parent: "Node | None" = None):
        """Depth first, a directory before its children: `(node, parent)`."""
        yield self, parent
        for name in sorted(self.dirs):
            yield from self.dirs[name].walk(self)


def choose_spc(sizes: list[int], dir_entries: list[int]) -> int:
    """The smallest sectors-per-cluster whose cluster count fits FAT16."""
    for spc in (1, 2, 4, 8, 16, 32, 64):
        csz = spc * SECTOR
        used = sum(-(-s // csz) for s in sizes) + sum(-(-(n * 32) // csz) or 1 for n in dir_entries)
        if used + SLACK_CLUSTERS <= FAT16_MAX_CLUSTERS:
            return spc
    raise FormatError("the files do not fit a FAT16 volume (more than 2 GiB)")


def boot_sector(spc: int, clusters: int, fat_sectors: int, total: int, hidden: int, label: str) -> bytes:
    raw = bytearray(SECTOR)
    raw[0:3] = b"\xEB\x3C\x90"
    raw[3:11] = b"LAZYOS  "
    struct.pack_into("<HBHBHHBHHHI", raw, 11, SECTOR, spc, 1, 2, ROOT_ENTRIES,
                     total if total < 0x10000 else 0, 0xF8, fat_sectors, 63, 255, hidden)
    struct.pack_into("<I", raw, 32, 0 if total < 0x10000 else total)
    raw[36], raw[38] = 0x80, 0x29
    struct.pack_into("<I", raw, 39, VOLUME_ID)
    raw[43:54] = label.upper().ljust(11)[:11].encode("ascii")
    raw[54:62] = b"FAT16   "
    raw[510:512] = b"\x55\xAA"
    return bytes(raw)


def mbr(part_start: int, part_sectors: int, part_type: int) -> bytes:
    raw = bytearray(SECTOR)
    struct.pack_into("<I", raw, 440, DISK_SIGNATURE)
    raw[446:462] = bytes([0x80, 0xFE, 0xFF, 0xFF, part_type, 0xFE, 0xFF, 0xFF]) + \
        struct.pack("<II", part_start, part_sectors)
    raw[510:512] = b"\x55\xAA"
    return bytes(raw)


def format_image(out: Path, files: dict[str, Path], label: str = "LAZYOSESP",
                 part_start: int = 2048, part_type: int = 0x0E) -> dict:
    """Write `out`: an MBR, a gap to `part_start`, one FAT16 partition holding `files`.

    `files` maps a path inside the volume (`EFI/BOOT/BOOTX64.EFI`) to a source
    file. Returns the geometry (`spc`, `clusters`, `part_start`, `part_sectors`).
    """
    root = Node()
    for dest, source in files.items():
        root.add(dest.strip("/"), Path(source))
    nodes = list(root.walk())
    entry_counts = [node.prepare(parent is None) for node, parent in nodes]
    root_count = entry_counts[0]
    if root_count > ROOT_ENTRIES:
        raise FormatError(f"{root_count} root entries (the root holds {ROOT_ENTRIES})")
    sub = list(zip([node for node, _ in nodes[1:]], entry_counts[1:]))
    sizes = [src.stat().st_size for node, _ in nodes for src in node.files.values()]
    spc = choose_spc(sizes, entry_counts[1:])
    csz = spc * SECTOR

    # Allocate clusters: subdirectories first, then the files, each contiguous.
    next_cluster = 2
    chains: list[tuple[int, int]] = []  # (first, count)
    for node, n in sub:
        node.cluster, count = next_cluster, max(1, -(-(n * 32) // csz))
        chains.append((next_cluster, count))
        next_cluster += count
    file_clusters: dict[int, tuple[int, int]] = {}
    for node, _ in nodes:
        for name in sorted(node.files):
            size = node.files[name].stat().st_size
            count = -(-size // csz)
            file_clusters[id(node.files[name])] = (next_cluster if count else 0, count)
            if count:
                chains.append((next_cluster, count))
            next_cluster += count
    clusters = max(next_cluster - 2 + SLACK_CLUSTERS, FAT16_MIN_CLUSTERS)
    if clusters > FAT16_MAX_CLUSTERS:
        raise FormatError(f"{clusters} clusters: too many for FAT16")
    fat_sectors = -(-((clusters + 2) * 2) // SECTOR)
    root_sectors = ROOT_ENTRIES * 32 // SECTOR
    data_start = 1 + 2 * fat_sectors + root_sectors
    part_sectors = data_start + clusters * spc

    fat = [0] * (clusters + 2)
    fat[0], fat[1] = 0xFFF8, 0xFFFF
    for first, count in chains:
        for c in range(first, first + count):
            fat[c] = c + 1 if c < first + count - 1 else 0xFFFF

    def render(node: Node, parent: "Node | None") -> bytes:
        # The root is cluster 0, so a child of the root points `..` at 0 as FAT wants.
        entries = [] if parent is None else [dir_entry(b".          ", 0x10, node.cluster, 0),
                                             dir_entry(b"..         ", 0x10, parent.cluster, 0)]
        if parent is None:
            entries.append(dir_entry(label.upper().ljust(11)[:11].encode("ascii"), 0x08, 0, 0))
        for name, short, is_dir, long_needed in node.layout:
            if long_needed:
                entries += lfn_entries(name, short)
            if is_dir:
                entries.append(dir_entry(short, 0x10, node.dirs[name].cluster, 0))
            else:
                first, _ = file_clusters[id(node.files[name])]
                entries.append(dir_entry(short, 0x20, first, node.files[name].stat().st_size))
        return b"".join(entries)

    with open(out, "wb") as image:
        image.truncate((part_start + part_sectors) * SECTOR)
        image.write(mbr(part_start, part_sectors, part_type))
        base = part_start * SECTOR
        image.seek(base)
        image.write(boot_sector(spc, clusters, fat_sectors, part_sectors, part_start, label))
        fat_bytes = struct.pack(f"<{len(fat)}H", *fat).ljust(fat_sectors * SECTOR, b"\0")
        for copy in range(2):
            image.seek(base + (1 + copy * fat_sectors) * SECTOR)
            image.write(fat_bytes)
        image.seek(base + (1 + 2 * fat_sectors) * SECTOR)
        image.write(render(root, None).ljust(root_sectors * SECTOR, b"\0"))
        for node, parent in nodes[1:]:
            image.seek(base + (data_start + (node.cluster - 2) * spc) * SECTOR)
            image.write(render(node, parent))
        for node, _ in nodes:
            for source in node.files.values():
                first, count = file_clusters[id(source)]
                if count:
                    image.seek(base + (data_start + (first - 2) * spc) * SECTOR)
                    with open(source, "rb") as f:
                        shutil.copyfileobj(f, image, 1 << 20)
    return {"spc": spc, "clusters": clusters, "part_start": part_start, "part_sectors": part_sectors,
            "bytes": (part_start + part_sectors) * SECTOR}
