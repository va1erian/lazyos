#!/usr/bin/env python3
"""Redact the machine identifiers out of a `survey_linux_box.sh` directory.

    python tools/boot/redact_survey.py SRC [OUT]

Copies every text file of SRC to OUT (default `SRC-redacted`, never an existing
directory, never the .tar.gz) with the same markers the first Kaby Lake survey
used (docs/compat/kabylake/hw-survey): REDACTED-HOST, REDACTED-GUID,
REDACTED-MAC, REDACTED. Then it rescans OUT and lists every line that still
looks like an identifier, so a person reviews those instead of the whole set.

What is *not* redacted on purpose: all-zero and broadcast MACs, DMI placeholder
strings ("Default string", "To Be Filled By O.E.M."), PCI addresses, vendor and
device ids, memory maps. Review the output before committing it (B0.md step 1).
"""

import argparse
import re
import sys
from pathlib import Path

GUID = r"[0-9A-Fa-f]{8}(?:-[0-9A-Fa-f]{4}){3}-[0-9A-Fa-f]{12}"
MAC = r"(?<![0-9A-Fa-f:/])[0-9A-Fa-f]{2}(?::[0-9A-Fa-f]{2}){5}(?![0-9A-Fa-f:])"
PLACEHOLDERS = ("default string", "to be filled by o.e.m.", "not specified", "none", "unknown")

# (pattern, replacement). Order matters: GUIDs before the shorter forms.
RULES = [
    (re.compile(GUID), "REDACTED-GUID"),
    (re.compile(r"(?<![0-9A-Fa-f])[0-9a-f]{32}(?![0-9A-Fa-f])"), "REDACTED-ID"),
    # FAT volume id as blkid / grub print it: UUID="B7E9-CC1F", --fs-uuid B7E9CC1F.
    (re.compile(r'(UUID=")[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}(")'), r"\1REDACTED-UUID\2"),
    (re.compile(r"(?i)(fs[-_.]uuid\s+(?:--\S+\s+)*)[0-9A-F]{4}-?[0-9A-F]{4}\b"), r"\1REDACTED-UUID"),
    # A FAT volume id in a by-uuid path or systemd unit name (B7E9-CC1F, B7E9\\x2dCC1F).
    (re.compile(r"(?i)(by[-\\]x2duuid[-/]|by-uuid/)[0-9A-F]{4}(?:-|\\x2d)[0-9A-F]{4}\b"), r"\1REDACTED-UUID"),
    # Names derived from a MAC address (altname enx00f1f53de2d7, wlx28cdc403d635).
    (re.compile(r"\b(?:enx|wlx)[0-9a-f]{12}\b"), "REDACTED-IFNAME"),
    # lspci -vv extended capability: Device Serial Number 00-e0-4c-ff-...
    (re.compile(r"(Device Serial Number )(?:[0-9a-fA-F]{2}-){7}[0-9a-fA-F]{2}"), r"\1REDACTED"),
    # USB string descriptors, in lsusb and the kernel log.
    (re.compile(r"(SerialNumber: ).+"), r"\1REDACTED"),
    (re.compile(r"(\biSerial\s+[1-9]\d*\s+)(?!\d{4}:\d\d:\d\d\.\d).+"), r"\1REDACTED"),
    # efibootmgr -v dumps device paths as raw bytes, partition GUID included
    # (mixed-endian, so the GUID rule cannot see it).
    (re.compile(r"^(\s+(?:dp|data):)(?: [0-9a-f]{2}| /)+$", re.M), r"\1 REDACTED-BYTES"),
    # fdisk's disk identifier.
    (re.compile(r"(Disk identifier: )\S+"), r"\1REDACTED-GUID"),
]


IPV6 = re.compile(r"(?<![0-9A-Za-z:])(?:[0-9A-Fa-f]{0,4}:){2,7}[0-9A-Fa-f]{0,4}(?![0-9A-Za-z:])")


def ipv6_sub(match):
    text = match.group(0)
    # Keep ::1 and ::, which name no machine; drop anything with an interface id.
    if text in ("::", "::1") or not ("::" in text or text.count(":") == 7):
        return text
    return "REDACTED-IPV6"


def lsblk_serials(text):
    """Blank the SERIAL column of an `lsblk -o ...,SERIAL,...` table."""
    lines = text.split("\n")
    for i, line in enumerate(lines):
        if line.startswith("NAME") and "SERIAL" in line:
            start = line.index("SERIAL")
            heads = [m.start() for m in re.finditer(r"\S+", line)]
            end = next((h for h in heads if h > start), len(line))
            for j in range(i + 1, len(lines)):
                row = lines[j]
                if not row.strip() or row.startswith("#"):
                    break
                cell = row[start:end]
                if cell.strip():
                    lines[j] = row[:start] + "REDACTED".ljust(len(cell)) + row[end:]
            break
    return "\n".join(lines)


def mac_sub(match):
    text = match.group(0).lower()
    return text if text in ("00:00:00:00:00:00", "ff:ff:ff:ff:ff:ff") else "REDACTED-MAC"


def dmi_sub(match):
    value = match.group(2).strip()
    if value.lower() in PLACEHOLDERS or value.startswith("REDACTED"):
        return match.group(0)
    return match.group(1) + "REDACTED"


DMI = re.compile(r"((?:^|\s)(?:[A-Za-z]+_serial:|(?:Serial Number|Asset Tag):)\s*)(.*)$", re.M)


def hostname(src):
    """The host the survey names in README.txt ('... on <host> at <stamp>')."""
    try:
        text = (src / "README.txt").read_text(errors="replace")
    except OSError:
        return None
    found = re.search(r" on (\S+) at \d{8}-\d{4}", text)
    return found.group(1) if found else None


def redact(text, host):
    for pattern, repl in RULES:
        text = pattern.sub(repl, text)
    text = re.sub(MAC, mac_sub, text)
    text = IPV6.sub(ipv6_sub, text)
    text = lsblk_serials(text)
    text = DMI.sub(dmi_sub, text)
    if host and host not in ("localhost", "box"):
        text = re.sub(re.escape(host), "REDACTED-HOST", text)
    return text


# What the review pass flags: a value that still looks like an identifier.
# Only values, not the words "serial" or "uuid": those name features too.
SUSPECT = re.compile(
    GUID + "|" + MAC + r"|(?<![0-9a-f])[0-9a-f]{32}(?![0-9a-f])"
    r"|(?i:by[-\\]x2duuid[-/]|by-uuid/)[0-9A-F]{4}(?:-|\\x2d)[0-9A-F]{4}"
    r"|\b(?:enx|wlx)[0-9a-f]{12}\b"
    r"|(?<![0-9A-Za-z:])fe80:[0-9A-Fa-f:]+"
    r"|(?i:serial(?: ?number)?)\s*[:=]\s*"
    r"(?!REDACTED|Default string|To Be Filled|\d{1,2}(?!\d)|8250|\s*$)\S"
)


def review(out):
    leftovers = []
    for path in sorted(out.rglob("*")):
        if not path.is_file():
            continue
        for number, line in enumerate(path.read_text(errors="replace").splitlines(), 1):
            if SUSPECT.search(line.replace("00:00:00:00:00:00", "").replace("ff:ff:ff:ff:ff:ff", "")):
                leftovers.append(f"{path.relative_to(out)}:{number}: {line.strip()[:160]}")
    return leftovers



def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("src", type=Path)
    parser.add_argument("out", type=Path, nargs="?")
    args = parser.parse_args(argv)

    src = args.src
    if not src.is_dir():
        parser.error(f"{src} is not a directory")
    out = args.out or src.with_name(src.name.rstrip("/\\") + "-redacted")
    if out.exists():
        parser.error(f"{out} exists; refusing to overwrite")
    host = hostname(src)

    out.mkdir(parents=True)
    count = 0
    for path in sorted(src.iterdir()):
        if not path.is_file() or path.name.endswith((".tar.gz", ".tgz")):
            continue
        text = path.read_text(errors="replace")
        (out / path.name).write_text(redact(text, host), encoding="utf-8", newline="\n")
        count += 1
    print(f"redacted {count} files from {src} into {out} (host: {host or 'unknown'})")

    leftovers = review(out)
    if leftovers:
        print(f"\n{len(leftovers)} line(s) still look like identifiers; review them:")
        print("\n".join(leftovers))
        return 1
    print("review pass: nothing left that looks like an identifier")
    return 0


if __name__ == "__main__":
    sys.exit(main())
