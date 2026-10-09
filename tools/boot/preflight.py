#!/usr/bin/env python3
"""Check a LazyOS stick image before it is written to a real stick.

    python3 tools/boot/preflight.py [target/lazyos-usb.img] [--stick-bytes N]

Read-only. It re-derives from the bytes of the image everything the firmware
and the loader rely on, so a build that quietly produced a layout a strict
UEFI would refuse is caught on the host, not in front of the target PC:

* the MBR: boot signature, the three partitions of docs/usb-stick.md
  (0x20 BIOS stage 2, 0x0C active FAT, 0x83 home), home 1 MiB aligned, last
  and inside the file;
* the FAT boot partition (FAT16 today, FAT32 accepted): label, `\\EFI\\BOOT\\BOOTX64.EFI` (a PE32+ x86-64
  EFI application), `kernel-x86_64`, `ramdisk`, `boot-stage-3`, `boot-stage-4`;
* the ramdisk: its own MBR, `lazyos.cfg` on the FAT12 partition (root= and
  home= lines), an ext2 root;
* the home partition: an ext2 superblock with the label `lazyhome`.

Exit status 0 when every check passes. With `--stick-bytes` (the capacity of
the stick as `write_stick.py --list` shows it) it also checks the image fits.
`--json` prints the results as JSON for scripts.
"""

import argparse
import json
import struct
import sys
from pathlib import Path

SECTOR = 512
MIB = 1 << 20


class Image:
    def __init__(self, path):
        self.path = path
        self.size = path.stat().st_size
        self.f = open(path, "rb")

    def read(self, offset, length):
        self.f.seek(offset)
        return self.f.read(length)


def mbr_entries(sector):
    """The four MBR partition entries as dicts (empty ones omitted)."""
    out = []
    for i in range(4):
        e = sector[446 + 16 * i : 446 + 16 * (i + 1)]
        boot, ptype = e[0], e[4]
        start, count = struct.unpack("<II", e[8:16])
        if ptype and count:
            out.append(
                {"index": i + 1, "active": boot == 0x80, "type": ptype,
                 "start": start, "count": count}
            )
    return out


class Fat:
    """A small read-only FAT32/FAT16/FAT12 reader over a byte range."""

    def __init__(self, image, base):
        self.image, self.base = image, base
        bpb = image.read(base, SECTOR)
        (self.bps, self.spc, self.reserved, self.nfats, self.root_entries,
         total16, _media, fatsz16) = struct.unpack("<HBHBHHBH", bpb[11:24])
        total32 = struct.unpack("<I", bpb[32:36])[0]
        fatsz32 = struct.unpack("<I", bpb[36:40])[0]
        self.fatsz = fatsz16 or fatsz32
        total = total16 or total32
        self.root_dir_sectors = (self.root_entries * 32 + self.bps - 1) // self.bps
        data_sectors = total - self.reserved - self.nfats * self.fatsz - self.root_dir_sectors
        clusters = data_sectors // self.spc
        self.kind = "FAT12" if clusters < 4085 else "FAT16" if clusters < 65525 else "FAT32"
        self.root_cluster = struct.unpack("<I", bpb[44:48])[0] if self.kind == "FAT32" else 0
        label_at = 71 if self.kind == "FAT32" else 43
        self.label = bpb[label_at : label_at + 11].decode("ascii", "replace").rstrip()
        self.signature = bpb[510:512] == b"\x55\xaa"
        self.fat = image.read(base + self.reserved * self.bps, self.fatsz * self.bps)
        self.first_data = self.reserved + self.nfats * self.fatsz + self.root_dir_sectors

    def next_cluster(self, c):
        if self.kind == "FAT32":
            return struct.unpack("<I", self.fat[c * 4 : c * 4 + 4])[0] & 0x0FFFFFFF
        if self.kind == "FAT16":
            return struct.unpack("<H", self.fat[c * 2 : c * 2 + 2])[0]
        v = struct.unpack("<H", self.fat[c * 3 // 2 : c * 3 // 2 + 2])[0]
        return (v >> 4) if c & 1 else (v & 0x0FFF)

    def eoc(self, c):
        return c >= {"FAT32": 0x0FFFFFF8, "FAT16": 0xFFF8, "FAT12": 0xFF8}[self.kind]

    def chain(self, first):
        c, seen = first, set()
        while 2 <= c and not self.eoc(c) and c not in seen:
            seen.add(c)
            yield c
            c = self.next_cluster(c)

    def cluster_bytes(self, c):
        off = self.base + (self.first_data + (c - 2) * self.spc) * self.bps
        return self.image.read(off, self.spc * self.bps)

    def read_chain(self, first, size=None, limit=None):
        data = bytearray()
        for c in self.chain(first):
            data += self.cluster_bytes(c)
            if limit is not None and len(data) >= limit:
                break
        return bytes(data if size is None else data[:size])

    def dir_entries(self, first_cluster):
        """(name, attr, first_cluster, size, order) for each entry; LFN joined."""
        if first_cluster == 0 and self.kind != "FAT32":
            start = self.base + (self.reserved + self.nfats * self.fatsz) * self.bps
            raw = self.image.read(start, self.root_dir_sectors * self.bps)
        else:
            raw = b"".join(self.cluster_bytes(c) for c in self.chain(first_cluster))
        out, lfn = [], []
        for i in range(0, len(raw), 32):
            e = raw[i : i + 32]
            if e[0] == 0:
                break
            if e[0] == 0xE5:
                lfn = []
                continue
            if e[11] == 0x0F:
                part = e[1:11] + e[14:26] + e[28:32]
                lfn.insert(0, part.decode("utf-16le").split("\0")[0])
                continue
            if e[11] & 0x08:  # volume label
                lfn = []
                continue
            short = e[0:8].decode("ascii", "replace").rstrip()
            ext = e[8:11].decode("ascii", "replace").rstrip()
            name = "".join(lfn) or (short + ("." + ext if ext else ""))
            lfn = []
            hi, lo = struct.unpack("<H", e[20:22])[0], struct.unpack("<H", e[26:28])[0]
            out.append((name, e[11], (hi << 16) | lo, struct.unpack("<I", e[28:32])[0], i // 32))
        return out

    def find(self, path):
        """The (name, attr, cluster, size, order) of `path`, matching case-insensitively."""
        cluster = self.root_cluster
        entry = None
        for part in [p for p in path.split("/") if p]:
            entry = next((e for e in self.dir_entries(cluster) if e[0].lower() == part.lower()), None)
            if entry is None:
                return None
            cluster = entry[2]
        return entry

    def first_entries(self, path):
        """The first two names in a directory (must be '.' and '..' for strict firmware)."""
        entry = self.find(path) if path else None
        cluster = entry[2] if entry else self.root_cluster
        return [e[0] for e in self.dir_entries(cluster)[:2]]


class Report:
    def __init__(self):
        self.rows = []

    def check(self, name, ok, detail=""):
        self.rows.append({"check": name, "ok": bool(ok), "detail": detail})
        return ok

    def warn(self, name, detail):
        self.rows.append({"check": name, "ok": True, "warn": True, "detail": detail})


def check_pe(data, report):
    ok = data[:2] == b"MZ"
    pe_off = struct.unpack("<I", data[0x3C:0x40])[0] if ok and len(data) > 0x40 else 0
    ok = ok and data[pe_off : pe_off + 4] == b"PE\0\0"
    machine = struct.unpack("<H", data[pe_off + 4 : pe_off + 6])[0] if ok else 0
    magic = struct.unpack("<H", data[pe_off + 24 : pe_off + 26])[0] if ok else 0
    subsystem = struct.unpack("<H", data[pe_off + 24 + 68 : pe_off + 24 + 70])[0] if ok else 0
    report.check("BOOTX64.EFI is a PE32+ x86-64 EFI application",
                 ok and machine == 0x8664 and magic == 0x20B and subsystem == 10,
                 f"machine={machine:#x} magic={magic:#x} subsystem={subsystem}")


def ext2_label(image, offset):
    sb = image.read(offset + 1024, 1024)
    magic = struct.unpack("<H", sb[56:58])[0]
    return magic == 0xEF53, sb[120:136].split(b"\0")[0].decode("ascii", "replace")


def run(path, stick_bytes):
    image = Image(path)
    r = Report()
    mbr = image.read(0, SECTOR)
    r.check("MBR boot signature 55AA", mbr[510:512] == b"\x55\xaa")
    parts = mbr_entries(mbr)
    by_type = {p["type"]: p for p in parts}
    r.check("three MBR partitions (0x20, 0x0C, 0x83)",
            sorted(by_type) == [0x0C, 0x20, 0x83] and len(parts) == 3,
            ", ".join(f"{p['index']}:{p['type']:#04x}" for p in parts))
    if sorted(by_type) != [0x0C, 0x20, 0x83]:
        return r, image
    boot, home = by_type[0x0C], by_type[0x83]
    r.check("FAT partition is the active one", boot["active"] and not home["active"])
    r.check("home partition starts on a 1 MiB boundary", home["start"] * SECTOR % MIB == 0,
            f"LBA {home['start']}")
    if boot["start"] * SECTOR % MIB:
        r.warn("boot partition is not 1 MiB aligned",
               f"LBA {boot['start']}: the bootloader crate packs it after stage 2; firmware reads the BPB, not the alignment")
    end = (home["start"] + home["count"]) * SECTOR
    r.check("home partition is last and inside the image",
            home["start"] > boot["start"] and end <= image.size,
            f"ends at {end} of {image.size}")
    if stick_bytes:
        r.check("image fits the stick", image.size <= stick_bytes,
                f"image {image.size / MIB:.0f} MiB, stick {stick_bytes / MIB:.0f} MiB")

    # The firmware's view: the FAT32 ESP-by-another-name.
    fat = Fat(image, boot["start"] * SECTOR)
    r.check("boot partition is FAT16/FAT32 with a 55AA boot sector",
            fat.kind in ("FAT16", "FAT32") and fat.signature, fat.kind)
    if fat.kind != "FAT32" and boot["type"] == 0x0C:
        r.warn("partition type 0x0C (FAT32 LBA) holds a " + fat.kind + " volume",
               "firmware picks the driver from the BPB, so this is expected to boot; "
               "suspect it first if one firmware refuses the stick")
    hidden = struct.unpack("<I", image.read(boot["start"] * SECTOR + 28, 4))[0]
    if hidden != boot["start"]:
        r.warn("BPB hidden sectors != partition start",
               f"{hidden} vs {boot['start']}: UEFI ignores it; legacy BIOS boot code may not")
    r.check("FAT label is LAZYOS (upper case)", fat.label == "LAZYOS", repr(fat.label))
    efi = fat.find("EFI/BOOT/BOOTX64.EFI")
    if r.check("\\EFI\\BOOT\\BOOTX64.EFI exists", efi is not None):
        check_pe(fat.read_chain(efi[2], efi[3]), r)
        r.check("BOOTX64.EFI is not suspiciously small", efi[3] > 50_000, f"{efi[3]} bytes")
    for d in ("EFI", "EFI/BOOT"):
        r.check(f"{d}: '.' and '..' are the first entries", fat.first_entries(d) == [".", ".."],
                str(fat.first_entries(d)))
    sizes = {}
    for name in ("kernel-x86_64", "ramdisk", "boot-stage-3", "boot-stage-4"):
        e = fat.find(name)
        r.check(f"{name} on the boot partition", e is not None and e[3] > 0, f"{e[3]} bytes" if e else "missing")
        sizes[name] = e

    # The ramdisk: a whole-disk image with its own MBR.
    rd = sizes.get("ramdisk")
    if rd:
        data = fat.read_chain(rd[2], rd[3])
        rimg = type("Mem", (), {"read": lambda s, o, n: data[o : o + n], "size": len(data)})()
        rp = mbr_entries(data[:SECTOR])
        r.check("ramdisk has an MBR with two partitions", data[510:512] == b"\x55\xaa" and len(rp) == 2,
                ", ".join(f"{p['index']}:{p['type']:#04x}@{p['start']}" for p in rp))
        if len(rp) == 2:
            cfg_fat = Fat(rimg, rp[0]["start"] * SECTOR)
            cfg = cfg_fat.find("lazyos.cfg")
            if r.check("ramdisk: lazyos.cfg present", cfg is not None):
                text = cfg_fat.read_chain(cfg[2], cfg[3]).decode("ascii", "replace")
                r.check("lazyos.cfg names root=UUID= and home=LABEL=lazyhome",
                        "root=UUID=" in text and "home=LABEL=lazyhome" in text, text.strip().replace("\n", " | "))
            ok, label = ext2_label(rimg, rp[1]["start"] * SECTOR)
            r.check("ramdisk root is ext2 'lazyos'", ok and label == "lazyos", f"ext2={ok} label={label!r}")
            r.warn("ramdisk size", f"{len(data) / MIB:.0f} MiB (the BIOS loader reads it slowly; UEFI is fast)")

    ok, label = ext2_label(image, home["start"] * SECTOR)
    r.check("home partition is ext2 'lazyhome'", ok and label == "lazyhome", f"ext2={ok} label={label!r}")
    return r, image


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("image", nargs="?", default="target/lazyos-usb.img")
    ap.add_argument("--stick-bytes", type=int, default=0, help="stick capacity in bytes")
    ap.add_argument("--json", action="store_true")
    args = ap.parse_args()
    path = Path(args.image)
    if not path.is_file():
        print(f"preflight: {path} not found (build it: docs/usb-stick.md)", file=sys.stderr)
        return 2
    report, _ = run(path, args.stick_bytes)
    failed = [row for row in report.rows if not row["ok"]]
    if args.json:
        print(json.dumps({"image": str(path), "ok": not failed, "checks": report.rows}, indent=2))
    else:
        for row in report.rows:
            mark = "warn" if row.get("warn") else ("ok  " if row["ok"] else "FAIL")
            print(f"[{mark}] {row['check']}" + (f"  ({row['detail']})" if row["detail"] else ""))
        print("PREFLIGHT:PASS" if not failed else f"PREFLIGHT:FAIL ({len(failed)} failed)")
    return 0 if not failed else 1


if __name__ == "__main__":
    sys.exit(main())
