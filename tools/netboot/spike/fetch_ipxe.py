#!/usr/bin/env python3
"""Download the official prebuilt iPXE EFI binary for the Q0 spike.

    python -I tools/netboot/spike/fetch_ipxe.py --out target/netboot/ipxe

(`https://boot.ipxe.org/x86_64-efi/ipxe.efi`: the top-level `/ipxe.efi` is a 404.)

The download is untrusted data: it goes into a directory of its own (new or
empty, or holding only this tool's earlier output), is only read as bytes
(never run by this tool), must parse as a PE32+ x86-64 EFI application, and
its SHA-256 and the download date are written to `ipxe.sha256`. A later run
that downloads different bytes refuses to replace the file unless `--force`,
so the digest you tested stays the digest you have. When the bytes are
already there and `ipxe.sha256` is not, the record is written (an existing
record is never rewritten).

iPXE boots unauthenticated code over plain HTTP, by design: the registry's
files are fetched and run with no signature check (docs/netboot-plan.md,
decision 1 and risk 11). This is the optional, measurement-only path; the
signed path is the LazyOS boot OS, whose picker verifies `manifest.sig` before
it kexecs anything. So `--box-host` (and everything this tool writes for a
real machine) is for a lab network you control, never a shared one.

Also writes, from the templates the server uses (`scripts.py`):
`autoexec.ipxe` (the spike's, falls into the iPXE shell on failure),
`boot.ipxe.example` (the menu `serve.py` generates) and, with `--box-host`,
`autoexec.box.ipxe` (the plan's section 7 file for the real machine).
"""

from __future__ import annotations

import argparse
import datetime
import hashlib
import os
import sys
import urllib.request
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))  # `python -I` drops the script dir

import common  # noqa: E402
import preflight  # noqa: E402
import scripts  # noqa: E402

URL = "https://boot.ipxe.org/x86_64-efi/ipxe.efi"
BINARY = "ipxe.efi"
DIGEST_FILE = "ipxe.sha256"
#: The prebuilt binary is about 1 MiB; anything far larger is not it.
MAX_BYTES = 16 << 20
#: Files this tool writes; a directory holding anything else is not "its own".
OWN_FILES = {BINARY, DIGEST_FILE, "autoexec.ipxe", "autoexec.box.ipxe", "boot.ipxe.example"}


class FetchError(Exception):
    """A refusal: the download or the target directory is not acceptable."""


def check_directory(out: Path) -> None:
    """Refuse a directory that holds files other than this tool's own."""
    if not out.exists():
        return
    if not out.is_dir():
        raise FetchError(f"{out} is not a directory")
    stray = sorted(p.name for p in out.iterdir() if p.name not in OWN_FILES)
    if stray:
        raise FetchError(f"{out} holds files that are not this tool's ({', '.join(stray)}): "
                         "give the download a new, empty directory")


def check_efi_binary(data: bytes) -> None:
    """The download must be a PE32+ x86-64 EFI application (preflight's check)."""
    if not 0 < len(data) <= MAX_BYTES:
        raise FetchError(f"download is {len(data)} bytes (expected up to {MAX_BYTES})")
    report = preflight.Report()
    preflight.check_pe(data, report)
    row = report.rows[0]
    if not row["ok"]:
        raise FetchError(f"not a PE32+ x86-64 EFI application ({row['detail']})")


def decide(existing: str | None, new: str, force: bool) -> str:
    """What to do with the binary: `write`, `keep` (same bytes) or `replace` (forced).

    A different digest is refused with `FetchError` unless `force`.
    """
    if existing is None:
        return "write"
    if existing == new:
        return "keep"
    if force:
        return "replace"
    raise FetchError(f"{BINARY} already exists with a different digest "
                     f"({existing[:16]}... vs downloaded {new[:16]}...); pass --force to replace it")


def digest_record(digest: str, url: str, when: datetime.datetime) -> str:
    """`ipxe.sha256`: a `sha256sum -c` line, then where and when it came from."""
    return f"{digest}  {BINARY}\n# downloaded {when:%Y-%m-%d %H:%M:%SZ} from {url}\n"


def download(url: str, timeout: float = 60.0) -> bytes:
    """GET `url` (certificates verified), at most `MAX_BYTES`."""
    with urllib.request.urlopen(url, timeout=timeout) as response:  # noqa: S310 (https, fixed URL)
        data = response.read(MAX_BYTES + 1)
    if len(data) > MAX_BYTES:
        raise FetchError(f"{url} is larger than {MAX_BYTES} bytes")
    return data


def write_atomic(path: Path, data: bytes) -> None:
    tmp = path.with_name(path.name + ".part")
    tmp.write_bytes(data)
    os.replace(tmp, path)


def write_templates(out: Path, guest_host: str, port: int, box_host: str | None) -> list[str]:
    """The scripts that go next to the binary; the names written."""
    reg = scripts.registry_url(guest_host, port)
    files = {
        "autoexec.ipxe": scripts.autoexec(reg, "shell"),
        "boot.ipxe.example": scripts.boot_menu(reg, "example build (see serve.py)"),
    }
    if box_host:
        files["autoexec.box.ipxe"] = scripts.autoexec(scripts.registry_url(box_host, port), "exit")
    for name, text in files.items():
        (out / name).write_text(text, newline="\n")
    return list(files)


def fetch(out: Path, url: str, force: bool, source: Path | None, now: datetime.datetime) -> tuple[str, str]:
    """Download (or read `source`), validate, and store; `(action, sha256)`."""
    check_directory(out)
    data = source.read_bytes() if source else download(url)
    check_efi_binary(data)
    digest = hashlib.sha256(data).hexdigest()
    target = out / BINARY
    action = decide(common.sha256_file(target) if target.exists() else None, digest, force)
    out.mkdir(parents=True, exist_ok=True)
    record = out / DIGEST_FILE
    if action != "keep":
        write_atomic(target, data)
        record.write_text(digest_record(digest, source.as_posix() if source else url, now))
    elif not record.exists():  # same bytes, but the record is gone: restore it, never rewrite one
        record.write_text(digest_record(digest, source.as_posix() if source else url, now))
    return action, digest


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--out", default=str(common.DEFAULT_IPXE),
                    help="a new or empty directory for the download (default %(default)s)")
    ap.add_argument("--url", default=URL)
    ap.add_argument("--from-file", help="use this local copy instead of downloading")
    ap.add_argument("--force", action="store_true", help="replace a binary with a different digest")
    ap.add_argument("--guest-host", default=common.SLIRP_HOST,
                    help="registry host as the guest sees it (default %(default)s, slirp's host)")
    ap.add_argument("--port", type=int, default=8420)
    ap.add_argument("--box-host", help="also write autoexec.box.ipxe for the real machine, "
                                       "pointing at this host (the dev machine's LAN address)")
    args = ap.parse_args(argv)
    out = Path(args.out)
    try:
        now = datetime.datetime.now(datetime.timezone.utc)
        action, digest = fetch(out, args.url, args.force,
                               Path(args.from_file) if args.from_file else None, now)
        names = write_templates(out, args.guest_host, args.port, args.box_host)
    except (FetchError, OSError, ValueError) as exc:
        print(f"fetch_ipxe: {exc}", file=sys.stderr)
        return 1
    size = (out / BINARY).stat().st_size
    print(f"{BINARY}: {action}, {size} bytes, sha256 {digest}")
    print(f"wrote {out / DIGEST_FILE} and {', '.join(names)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
