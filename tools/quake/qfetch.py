"""Fetch the Quake port's three third-party inputs, each pinned.

* the ``quake-srp`` engine source (GPL-2.0-or-later), a GitHub archive of
  the revision :data:`QUAKE_SRP_REV`, extracted to
  ``target/quake/quake-srp-<rev>/``;
* id's freely redistributable shareware Quake 1.06 (the ``quake106.zip``
  the port's own ``ci/fetch_shareware.sh`` fetches), from which
  ``ID1/PAK0.PAK`` and the shareware licence are extracted into
  ``target/quake/quake-srp-<rev>/quake-data/`` — the crate-adjacent place
  the assembled build's upstream tests read (``common.rs``'s default
  path), exactly where quake-srp's own CI keeps it.

Nothing here is committed: a download goes to a temporary name and is
moved into place only once it matches its digest, and a cached file is
re-verified, so a truncated or tampered file never is used (the same
rules ``tools/doom/fetch.py`` runs under). Every failure returns ``None``
with the reason on stderr; the caller decides whether that is fatal.
"""

from __future__ import annotations

import hashlib
import shutil
import subprocess
import sys
import tarfile
import urllib.request
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent
CACHE = ROOT / "target" / "quake"

#: quake-srp at a reviewed commit; bumping it means updating the digest too.
QUAKE_SRP_REV = "f55b777e4eb3b6e06e516ae44ad37ef0958fbe7e"
QUAKE_SRP_SHA256 = "6031cc5136181b41112af51819a9a568a00720f96ba8cd721823f9db22e44374"
QUAKE_SRP_URL = f"https://github.com/terrapapagalli1516/quake-srp/archive/{QUAKE_SRP_REV}.tar.gz"

#: id's shareware 1.06, from the official archive mirror the upstream port
#: fetches from; both digests are the upstream script's pins.
QUAKE106_URL = (
    "https://raw.githubusercontent.com/Jason2Brownlee/QuakeOfficialArchive/"
    "main/bin/quake106.zip"
)
QUAKE106_SHA256 = "ec6c9d34b1ae0252ac0066045b6611a7919c2a0d78a3a66d9387a8f597553239"
PAK0_SHA256 = "35a9c55e5e5a284a159ad2a62e0e8def23d829561fe2f54eb402dbc0a9a946af"

#: Marks a fully extracted tree; a tree without it is partial and rebuilt.
EXTRACTED_MARK = ".lazyos-extracted"


def log(message: str) -> None:
    print(f"quake: {message}", file=sys.stderr)


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for block in iter(lambda: handle.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def download(url: str, dest: Path, digest: str) -> bool:
    """`dest` holding `url`'s bytes with SHA-256 `digest`; cached when valid."""
    if dest.is_file():
        if sha256(dest) == digest:
            return True
        log(f"cached {dest.name} fails the digest check; discarding")
        dest.unlink()
    dest.parent.mkdir(parents=True, exist_ok=True)
    partial = dest.with_suffix(dest.suffix + ".part")
    log(f"downloading {url}")
    try:
        urllib.request.urlretrieve(url, partial)
        if sha256(partial) != digest:
            raise ValueError("SHA-256 does not match the pinned digest")
        partial.replace(dest)
    except Exception as error:  # noqa: BLE001 - any failure is "unavailable"
        partial.unlink(missing_ok=True)
        log(f"download failed: {error}")
        return False
    return True


def quake_srp() -> Path | None:
    """The fetched port tree: `quake-rs/` and `quake-wasm/` beside the
    assembled crate."""
    tree = CACHE / f"quake-srp-{QUAKE_SRP_REV}"
    if (tree / EXTRACTED_MARK).is_file():
        return tree
    archive = CACHE / f"quake-srp-{QUAKE_SRP_REV}.tar.gz"
    if not download(QUAKE_SRP_URL, archive, QUAKE_SRP_SHA256):
        return None
    staging = CACHE / ".extract-quake-srp"
    shutil.rmtree(staging, ignore_errors=True)
    shutil.rmtree(tree, ignore_errors=True)
    try:
        staging.mkdir(parents=True)
        with tarfile.open(archive, "r:gz") as tar:
            for member in tar.getmembers():
                # Plain files and directories only: no links, no devices.
                if not (member.isfile() or member.isdir()):
                    continue
                tar.extract(member, staging, set_attrs=False)
        roots = [entry for entry in staging.iterdir() if (entry / "quake-rs").is_dir()]
        if len(roots) != 1:
            raise ValueError("the archive is not one quake-srp checkout")
        if not (roots[0] / "quake-wasm" / "src" / "sys.rs").is_file():
            raise ValueError("quake-wasm/src is missing")
        roots[0].rename(tree)
        (tree / EXTRACTED_MARK).write_text("ok\n", encoding="utf-8")
    except Exception as error:  # noqa: BLE001 - a bad archive is "unavailable"
        log(f"extracting quake-srp failed: {error}")
        shutil.rmtree(tree, ignore_errors=True)
        return None
    finally:
        shutil.rmtree(staging, ignore_errors=True)
    return tree


def extract_tar() -> str:
    """A libarchive `bsdtar`, needed for the LZH archive: Debian's
    `libarchive-tools` installs it as `bsdtar`, Windows' own `tar` is
    libarchive too. GNU tar cannot read the format and gets refused with
    its name, so the failure says what to install."""
    for name in ("bsdtar", "tar"):
        found = shutil.which(name)
        if found is None:
            continue
        version = subprocess.run([found, "--version"], capture_output=True, text=True)
        if "libarchive" in (version.stdout + version.stderr).lower():
            return found
    raise ValueError(
        "no bsdtar (libarchive) on PATH: install libarchive-tools; "
        "GNU tar cannot read the shareware zip's LZH archive"
    )


def shareware(tree: Path) -> Path | None:
    """`tree/quake-data/` holding `ID1/PAK0.PAK`, the shareware licence
    and id's archive itself."""
    data = tree / "quake-data"
    pak = data / "ID1" / "PAK0.PAK"
    if pak.is_file() and sha256(pak) == PAK0_SHA256 and (data / "SLICNSE.TXT").is_file():
        return data
    archive = CACHE / "quake106.zip"
    if not download(QUAKE106_URL, archive, QUAKE106_SHA256):
        return None
    staging = CACHE / ".extract-quake106"
    shutil.rmtree(staging, ignore_errors=True)
    try:
        staging.mkdir(parents=True)
        # The zip holds an LZH archive (`resource.1`), which `bsdtar`
        # unpacks in one call — Windows' own `tar` and Debian's
        # `libarchive-tools` are both this.
        with zipfile.ZipFile(archive) as zf:
            for name in ("resource.1",):
                with zf.open(name) as src, open(staging / name, "wb") as dst:
                    shutil.copyfileobj(src, dst)
        tar = extract_tar()
        extract = subprocess.run(
            [tar, "-xf", "resource.1", "ID1/PAK0.PAK", "SLICNSE.TXT"],
            cwd=staging,
            capture_output=True,
            text=True,
        )
        if extract.returncode != 0:
            raise ValueError(f"bsdtar failed: {extract.stderr.strip()}")
        if not (staging / "ID1" / "PAK0.PAK").is_file():
            raise ValueError("the LZH archive held no ID1/PAK0.PAK")
        digest = sha256(staging / "ID1" / "PAK0.PAK")
        if digest != PAK0_SHA256:
            raise ValueError(f"PAK0.PAK's digest is {digest}, not the pinned one")
        shutil.rmtree(data, ignore_errors=True)
        data.mkdir(parents=True)
        (data / "ID1").mkdir()
        shutil.copyfile(staging / "ID1" / "PAK0.PAK", pak)
        shutil.copyfile(staging / "SLICNSE.TXT", data / "SLICNSE.TXT")
        shutil.copyfile(archive, data / "quake106.zip")
    except Exception as error:  # noqa: BLE001 - a bad archive is "unavailable"
        log(f"extracting the shareware pak failed: {error}")
        return None
    finally:
        shutil.rmtree(staging, ignore_errors=True)
    return data


def sha256_paths(root: Path) -> str:
    """A fingerprint of every file under `root`: name and digest, sorted —
    the overlay's assembled-crate rebuild key."""
    digest = hashlib.sha256()
    for path in sorted(root.rglob("*")):
        if path.is_file():
            rel = path.relative_to(root).as_posix()
            digest.update(rel.encode("utf-8"))
            digest.update(sha256(path).encode("utf-8"))
    return digest.hexdigest()
