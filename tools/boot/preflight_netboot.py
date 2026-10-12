"""`preflight.py --netboot DIR`: the stick checks on the loose files of
`target/netboot/` (docs/netboot-plan.md N1), plus the manifest.

The directory is what `LAZYOS_NETBOOT=1 cargo build` writes: `kernel-x86_64`,
`ramdisk`, `bootx64.efi`, `boot.json`, `manifest.json` and, when the build had
`LAZYOS_NETBOOT_KEY`, `manifest.sig`. The registry's `commit` accepts a build
only when the signature verifies and every digest matches, so this checks the
shapes it will meet (the signature itself needs the public key, which only the
registry and the boot OS hold).
"""

import hashlib
import json
import re
import struct
from pathlib import Path

from preflight import Image, Report, check_pe, check_ramdisk

PAYLOAD = ("kernel-x86_64", "ramdisk", "bootx64.efi", "boot.json")
ID_RE = re.compile(r"^(\d{8}-\d{6})-([0-9a-f]{8})(-dirty)?$")
IMAGE_ID_RE = re.compile(r"^[0-9A-Za-z._-]{1,64}$")
SHA_RE = re.compile(r"^[0-9a-f]{64}$")
SIG_RE = re.compile(r"^[0-9a-f]{128}\n$")
TIME_RE = re.compile(r"^(\d{4})-(\d\d)-(\d\d)T(\d\d):(\d\d):(\d\d)Z$")


def digest(path):
    """(size, lowercase hex SHA-256) of a file, read in 1 MiB steps."""
    sha, size = hashlib.sha256(), 0
    with open(path, "rb") as f:
        while chunk := f.read(1 << 20):
            sha.update(chunk)
            size += len(chunk)
    return size, sha.hexdigest()


def image_id_of(cfg_text):
    """The value of the `image.id=` line of a `lazyos.cfg`, or None."""
    for line in cfg_text.splitlines():
        key, _, value = line.partition("=")
        if key.strip() == "image.id":
            return value.strip()
    return None


def check_manifest(manifest, directory, r):
    """The manifest's fields, and its sizes and digests against the files."""
    if not r.check("manifest.json is an object", isinstance(manifest, dict)):
        return
    ident = manifest.get("id")
    match = ID_RE.match(ident) if isinstance(ident, str) else None
    r.check("manifest id is <yyyymmdd-HHMMSS>-<rev8>[-dirty]", match is not None, repr(ident))
    revision = manifest.get("revision")
    r.check("manifest revision is 40 hex digits or unknown",
            isinstance(revision, str) and (revision == "unknown" or re.fullmatch(r"[0-9a-f]{40}", revision)),
            repr(revision))
    dirty = manifest.get("dirty")
    r.check("manifest dirty is a bool that agrees with the id",
            isinstance(dirty, bool) and match is not None and dirty == bool(match.group(3)),
            f"dirty={dirty!r}")
    if match is not None and isinstance(revision, str):
        want = "00000000" if revision == "unknown" else revision[:8]
        r.check("the id's revision part is the manifest's revision", match.group(2) == want,
                f"{match.group(2)} vs {want}")
    built = manifest.get("built_at")
    when = TIME_RE.match(built) if isinstance(built, str) else None
    r.check("manifest built_at is ISO 8601 UTC (2026-10-12T09:30:00Z)", when is not None, repr(built))
    if match is not None and when is not None:
        r.check("the id's time is the manifest's built_at",
                match.group(1) == "{}{}{}-{}{}{}".format(*when.groups()), f"{match.group(1)} vs {built}")
    host = manifest.get("host")
    r.check("manifest host is a non-empty string", isinstance(host, str) and host != "", repr(host))
    switches = manifest.get("switches")
    r.check("manifest switches are the sorted LAZYOS_* names",
            isinstance(switches, list)
            and all(isinstance(s, str) and s.startswith("LAZYOS_") for s in switches)
            and switches == sorted(set(switches)), repr(switches))

    files = manifest.get("files")
    if not r.check("manifest lists exactly the four payload files",
                   isinstance(files, dict) and sorted(files) == sorted(PAYLOAD),
                   repr(sorted(files)) if isinstance(files, dict) else repr(files)):
        return
    for name in PAYLOAD:
        entry, path = files[name], directory / name
        if not r.check(f"{name}: manifest entry is {{size, sha256}}",
                       isinstance(entry, dict) and set(entry) == {"size", "sha256"}
                       and type(entry["size"]) is int and isinstance(entry["sha256"], str)
                       and SHA_RE.match(entry["sha256"]) is not None, repr(entry)):
            continue
        if path.is_file():
            size, sha = digest(path)
            r.check(f"{name}: size and sha256 match the manifest",
                    (size, sha) == (entry["size"], entry["sha256"]),
                    f"file {size} bytes {sha[:12]}..., manifest {entry['size']} bytes {entry['sha256'][:12]}...")


def check_netboot(directory, r):
    """Every check on the directory's files, appended to `r`."""
    files = {name: directory / name for name in (*PAYLOAD, "manifest.json")}
    for name, path in files.items():
        r.check(f"{name} exists and is not empty", path.is_file() and path.stat().st_size > 0,
                f"{path.stat().st_size} bytes" if path.is_file() else "missing")
    if not all(path.is_file() for path in files.values()):
        return

    head = files["kernel-x86_64"].read_bytes()[:20]
    r.check("kernel-x86_64 is an x86-64 ELF",
            head[:4] == b"\x7fELF" and head[4] == 2 and int.from_bytes(head[18:20], "little") == 0x3E,
            head[:4].hex())
    check_pe(files["bootx64.efi"].read_bytes(), r)
    try:
        json.loads(files["boot.json"].read_text(encoding="utf-8"))
        r.check("boot.json is JSON", True)
    except (ValueError, UnicodeDecodeError) as exc:
        r.check("boot.json is JSON", False, str(exc))

    ramdisk = Image(files["ramdisk"])
    try:
        cfg = check_ramdisk(ramdisk, r, size_note="downloaded whole into the picker's RAM")
    finally:
        ramdisk.f.close()
    image_id = image_id_of(cfg)
    r.check("lazyos.cfg has image.id=<id> of 1 to 64 of [0-9A-Za-z._-]",
            image_id is not None and IMAGE_ID_RE.match(image_id) is not None, repr(image_id))

    try:
        manifest = json.loads(files["manifest.json"].read_text(encoding="utf-8"))
    except (ValueError, UnicodeDecodeError) as exc:
        r.check("manifest.json is JSON", False, str(exc))
        return
    check_manifest(manifest, directory, r)
    if isinstance(manifest, dict):
        r.check("the ramdisk's image.id is the manifest's id", image_id == manifest.get("id"),
                f"{image_id!r} vs {manifest.get('id')!r}")

    sig = directory / "manifest.sig"
    if sig.is_file():
        text = sig.read_bytes().decode("ascii", "replace")
        r.check("manifest.sig is 128 lowercase hex digits and a newline", SIG_RE.match(text) is not None,
                f"{len(text)} characters")
    else:
        r.warn("manifest.sig is missing",
               "unsigned: the registry refuses it and the picker will not boot it; build with LAZYOS_NETBOOT_KEY=<key.sec>")


def run(directory):
    """Run the netboot checks; a malformed structure becomes a failed check."""
    report = Report()
    try:
        check_netboot(Path(directory), report)
    except (ValueError, ArithmeticError, IndexError, OSError, struct.error) as exc:
        report.check("netboot files are readable", False, f"{type(exc).__name__}: {exc}")
    return report
