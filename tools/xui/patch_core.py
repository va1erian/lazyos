"""Patch the pinned upstream ``xui-core`` for the xui app build (issue #339).

``xui-app`` pins ``xui-core`` from git. The LazyOS fixes to it are small, so
instead of vendoring the whole crate (~350 KB, unlike the vendored
``xui-canvas``) the repository keeps one diff, ``xui-app/patches/xui-core.patch``.
:func:`prepare` copies cargo's checkout of the pinned revision into
``target/xui-patched/xui-core``, applies the diff there, and :func:`config_arg`
is the ``cargo --config`` that ``[patch]``es the git dependency onto the copy.

Building through ``--config`` rewrites one line of ``xui-app/Cargo.lock`` (a
path package has no ``source``), so :func:`preserve_lock` restores the file
afterwards and a plain ``cargo`` build of the app still resolves from git.
"""

from __future__ import annotations

import contextlib
import hashlib
import json
import os
import shutil
import subprocess
from pathlib import Path
from typing import Iterator

UPSTREAM = "https://github.com/va1erian/xui"
CRATE = "xui-core"
STAMP = ".lazyos-patch"


class PatchError(RuntimeError):
    """The pinned crate could not be located or the diff did not apply."""


def _checkout(app: Path) -> tuple[Path, str]:
    """Cargo's checkout of the pinned crate, and its package id."""
    metadata = subprocess.run(
        ["cargo", "metadata", "--manifest-path", str(app / "Cargo.toml"), "--format-version", "1"],
        capture_output=True,
        text=True,
    )
    if metadata.returncode != 0:
        raise PatchError(f"cargo metadata failed: {metadata.stderr.strip()[-500:]}")
    for package in json.loads(metadata.stdout)["packages"]:
        source = package.get("source") or ""
        if package["name"] == CRATE and source.startswith(f"git+{UPSTREAM}"):
            return Path(package["manifest_path"]).parent, package["id"]
    raise PatchError(f"{CRATE} from {UPSTREAM} is not in the xui-app dependency graph")


def prepare(root: Path, app: Path) -> Path:
    """Return a patched copy of the pinned crate, rebuilding it only when the
    diff or the pinned revision changed (a fresh copy would make cargo
    recompile the crate on every build)."""
    patch = (app / "patches" / f"{CRATE}.patch").read_text(encoding="utf-8")
    # A Windows checkout may carry CRLF; cargo's checkout of upstream is LF.
    patch = patch.replace("\r\n", "\n")
    source, package_id = _checkout(app)
    stamp = hashlib.sha256(f"{package_id}\n{patch}".encode()).hexdigest()

    dest = root / "target" / "xui-patched" / CRATE
    stamp_file = dest / STAMP
    if stamp_file.is_file() and stamp_file.read_text(encoding="utf-8") == stamp:
        return dest

    if dest.exists():
        shutil.rmtree(dest)
    shutil.copytree(source, dest, ignore=shutil.ignore_patterns(".cargo-ok", "target"))
    # The copy lives inside this repository's worktree; stop git's repository
    # discovery at its parent so `git apply` patches the copy like `patch -p1`
    # instead of resolving paths against the LazyOS repository root.
    env = dict(os.environ, GIT_CEILING_DIRECTORIES=str(dest.parent))
    # Bytes, not text: text-mode pipes on Windows would turn LF back into CRLF.
    applied = subprocess.run(
        ["git", "apply", "--verbose", "-"],
        cwd=dest,
        input=patch.encode("utf-8"),
        capture_output=True,
        env=env,
    )
    if applied.returncode != 0:
        detail = applied.stderr.decode("utf-8", "replace").strip()
        raise PatchError(f"{CRATE}.patch does not apply: {detail}")
    stamp_file.write_text(stamp, encoding="utf-8")
    return dest


def config_arg(crate_dir: Path) -> str:
    """The ``cargo --config`` value that patches the git crate onto the copy."""
    path = crate_dir.resolve().as_posix()
    return f'patch."{UPSTREAM}".{CRATE}.path="{path}"'


@contextlib.contextmanager
def preserve_lock(app: Path) -> Iterator[None]:
    """Restore ``Cargo.lock`` after a build that patched a git dependency."""
    lock = app / "Cargo.lock"
    original = lock.read_bytes()
    try:
        yield
    finally:
        if lock.read_bytes() != original:
            lock.write_bytes(original)
