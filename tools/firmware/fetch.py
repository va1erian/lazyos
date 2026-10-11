#!/usr/bin/env python3
"""Fetch the device firmware an image installs, each file pinned by SHA-256.

The pins are ``tools/firmware/manifest.txt`` (name, build switch, digest,
size, licence, commit-pinned URL). Files land in ``target/firmware/<name>``,
where the image build (``build_support/firmware_embed.rs``) checks them again
and places them at ``/system/share/firmware/<name>`` (``fhs::FIRMWARE``).
Firmware is never committed and never compiled into a program
(docs/wifi-prerequisites-plan.md 3.8).

    python tools/firmware/fetch.py                 # every file (the `wifi` ones today)
    python tools/firmware/fetch.py --switch wifi   # only what LAZYOS_WIFI=1 installs
    python tools/firmware/fetch.py --check         # download afresh and verify the pins

A download goes to a temporary name and is moved into place only once its
size and digest match; a cached file is re-verified, and discarded when it
does not match (the BusyBox and Doom fetchers' rules). Exits non-zero, with
the reason on stderr, when any file is unavailable.
"""

from __future__ import annotations

import argparse
import hashlib
import sys
import tempfile
import urllib.request
from dataclasses import dataclass
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
MANIFEST = HERE / "manifest.txt"
CACHE = ROOT / "target" / "firmware"
#: Some hosts (git.kernel.org) refuse Python's default agent.
USER_AGENT = "lazyos-firmware-fetch/1"
SEGMENT_BYTES = set(b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789._-")


@dataclass(frozen=True)
class Blob:
    name: str
    switch: str
    sha256: str
    size: int
    licence: str
    url: str


def log(message: str) -> None:
    print(f"firmware: {message}", file=sys.stderr)


def check_name(name: str) -> None:
    """`libs/fwload`'s rule: `vendor/file`, plain segments, nothing hidden."""
    segments = name.split("/")
    if len(segments) < 2 or any(not s or s.startswith(".") or not set(s.encode()) <= SEGMENT_BYTES
                                for s in segments):
        raise ValueError(f"`{name}` is not a vendor-relative firmware name")


def parse_manifest(text: str) -> list[Blob]:
    """Every line of a manifest; a malformed one raises ValueError."""
    blobs: list[Blob] = []
    for number, line in enumerate(text.splitlines(), 1):
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        fields = [field.strip() for field in line.split("|")]
        if len(fields) != 6:
            raise ValueError(f"line {number}: expected 6 fields, got {len(fields)}")
        name, switch, digest, size, licence, url = fields
        check_name(name)
        check_name(licence)
        if len(digest) != 64 or any(c not in "0123456789abcdef" for c in digest):
            raise ValueError(f"line {number}: the digest is not 64 lower-case hex digits")
        if not size.isdigit() or int(size) == 0:
            raise ValueError(f"line {number}: the size is not a positive number")
        if not url.startswith("https://"):
            raise ValueError(f"line {number}: the URL is not https")
        if any(blob.name == name for blob in blobs):
            raise ValueError(f"line {number}: {name} is listed twice")
        blobs.append(Blob(name, switch, digest, int(size), licence, url))
    for blob in blobs:
        if not any(other.name == blob.licence for other in blobs):
            raise ValueError(f"{blob.name}: its licence {blob.licence} is not listed")
    return blobs


def load_manifest(path: Path = MANIFEST) -> list[Blob]:
    return parse_manifest(path.read_text(encoding="utf-8"))


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for block in iter(lambda: handle.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def matches(path: Path, blob: Blob) -> bool:
    return path.is_file() and path.stat().st_size == blob.size and sha256(path) == blob.sha256


def download(blob: Blob, dest: Path) -> bool:
    """`dest` holding `blob`'s pinned bytes; a valid cached file is kept."""
    if dest.is_file():
        if matches(dest, blob):
            return True
        log(f"cached {blob.name} fails the size or digest check; discarding")
        dest.unlink()
    dest.parent.mkdir(parents=True, exist_ok=True)
    partial = dest.with_name(dest.name + ".part")
    log(f"downloading {blob.name} from {blob.url}")
    try:
        request = urllib.request.Request(blob.url, headers={"User-Agent": USER_AGENT})
        with urllib.request.urlopen(request, timeout=60) as response, partial.open("wb") as out:
            # One byte past the pinned size is enough to know it is wrong.
            data = response.read(blob.size + 1)
            out.write(data)
        if not matches(partial, blob):
            raise ValueError(f"{partial.stat().st_size} bytes / SHA-256 {sha256(partial)} do not "
                             f"match the pinned {blob.size} bytes / {blob.sha256}")
        partial.replace(dest)
    except Exception as error:  # noqa: BLE001 - any failure is "unavailable"
        partial.unlink(missing_ok=True)
        log(f"{blob.name}: download failed: {error}")
        return False
    return True


def wanted(blobs: list[Blob], switch: str | None) -> list[Blob]:
    """The files a switch installs, with the licences they need."""
    chosen = [blob for blob in blobs if switch is None or blob.switch == switch]
    licences = {blob.licence for blob in chosen}
    return chosen + [blob for blob in blobs if blob.name in licences and blob not in chosen]


def fetch(blobs: list[Blob], cache: Path = CACHE) -> bool:
    """Every blob in the cache, verified; False if any is unavailable."""
    ok = True
    for blob in blobs:
        ok = download(blob, cache / blob.name) and ok
    return ok


def check(blobs: list[Blob]) -> bool:
    """Download every blob afresh (no cache) and verify its pins."""
    results = []
    with tempfile.TemporaryDirectory(prefix="lazyos-fw-") as scratch:
        for blob in blobs:
            results.append(download(blob, Path(scratch) / blob.name))
    for blob, good in zip(blobs, results):
        print(f"{'OK  ' if good else 'FAIL'} {blob.name} {blob.size} {blob.sha256}")
    return all(results)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--switch", help="only the files this build switch installs (`wifi`)")
    parser.add_argument("--check", action="store_true",
                        help="download every file afresh and verify the pins (no cache)")
    parser.add_argument("--manifest", type=Path, default=MANIFEST, help=argparse.SUPPRESS)
    parser.add_argument("--cache", type=Path, default=CACHE, help=argparse.SUPPRESS)
    args = parser.parse_args(argv)
    try:
        blobs = wanted(load_manifest(args.manifest), args.switch)
    except (OSError, ValueError) as error:
        log(f"{args.manifest}: {error}")
        return 2
    if not blobs:
        log(f"no firmware for switch `{args.switch}`")
        return 2
    if args.check:
        return 0 if check(blobs) else 1
    if not fetch(blobs, args.cache):
        return 1
    log(f"{len(blobs)} file(s) ready in {args.cache}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
