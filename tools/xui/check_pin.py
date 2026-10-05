#!/usr/bin/env python3
"""Check that every manifest and lockfile pins one xui revision (W5.5).

lazyOS builds xui from git in several standalone workspaces (`xui-app`,
`doom`, `lazyrad-os`), each with its own `Cargo.toml` and `Cargo.lock`. Two
revisions in one image mean two copies of `xui_core` whose types do not mix,
and a bump that misses one workspace is easy to make. This script finds every
`va1erian/xui` git dependency (including `[patch]` entries and the `www.`
alias a patch uses to point at the same repository) and every xui package a
lockfile resolved, and fails when they disagree or when a dependency has no
`rev`.

A workspace that cannot move yet is listed in `LAGGING` with the revision it
is held at and why; it must stay exactly there, and the entry must go once the
workspace catches up.

Usage::

    python tools/xui/check_pin.py            # the repository this file is in
    python tools/xui/check_pin.py --root DIR

Exit status 0 when the pin is consistent, 1 otherwise; the findings go to
stdout.
"""

from __future__ import annotations

import argparse
import os
import re
import sys
import tomllib
from dataclasses import dataclass
from pathlib import Path
from urllib.parse import parse_qs, urlsplit

ROOT = Path(__file__).resolve().parent.parent.parent

# Directories never searched: build output and other checkouts.
SKIP_DIRS = {".git", ".claude", "target", "node_modules"}

# The xui repository, with or without `www.` and `.git`.
XUI_URL = re.compile(r"^(?:git\+)?https://(?:www\.)?github\.com/va1erian/xui(?:\.git)?/?$")

# Workspaces held at an older revision: directory (relative to the root, `/`
# separated) -> (revision, reason).
LAGGING: dict[str, tuple[str, str]] = {}


@dataclass(frozen=True)
class Pin:
    """One place that names an xui revision."""

    path: str  # relative to the root, `/` separated
    what: str  # the dependency or package it pins
    rev: str | None  # None for a git dependency without `rev`
    # A lockfile entry whose resolved commit is not the `rev` it asked for: a
    # stale lock, always a problem whatever the other pins say.
    mismatch: str | None = None


def is_xui(url: str) -> bool:
    """Whether `url` (a manifest `git` key or a lockfile source without its
    query) is the xui repository."""
    return bool(XUI_URL.match(url))


def manifest_pins(path: str, manifest: dict) -> list[Pin]:
    """The xui git dependencies of a parsed `Cargo.toml`, wherever they sit:
    `[dependencies]` and friends, `[target.*.…]`, `[workspace.dependencies]`
    and `[patch.<source>]`."""
    tables: list[tuple[str, dict]] = []
    sections = ("dependencies", "dev-dependencies", "build-dependencies")
    for section in sections:
        tables.append((section, manifest.get(section, {})))
    for target, body in manifest.get("target", {}).items():
        for section in sections:
            tables.append((f"target.{target}.{section}", body.get(section, {})))
    tables.append(("workspace.dependencies", manifest.get("workspace", {}).get("dependencies", {})))
    for source, body in manifest.get("patch", {}).items():
        tables.append((f"patch.{source}", body))
    pins = []
    for table, deps in tables:
        for name, spec in deps.items():
            if isinstance(spec, dict) and is_xui(str(spec.get("git", ""))):
                pins.append(Pin(path, f"{table}.{name}", spec.get("rev")))
    return pins


def lock_pins(path: str, lock: dict) -> list[Pin]:
    """The xui packages a parsed `Cargo.lock` resolved, by the `rev` in their
    source (the commit after `#` must be that revision)."""
    pins = []
    for package in lock.get("package", []):
        source = package.get("source", "")
        url, _, commit = source.partition("#")
        parts = urlsplit(url)
        if not is_xui(f"{parts.scheme}://{parts.netloc}{parts.path}"):
            continue
        rev = parse_qs(parts.query).get("rev", [None])[0]
        what = f"package {package.get('name')}"
        mismatch = None
        if rev is not None and commit and not commit.startswith(rev):
            mismatch = commit
        pins.append(Pin(path, what, rev, mismatch))
    return pins


def find_pins(root: Path) -> list[Pin]:
    """Every xui pin in the manifests and lockfiles under `root`."""
    pins = []
    for directory, subdirs, files in os.walk(root):
        subdirs[:] = sorted(d for d in subdirs if d not in SKIP_DIRS)
        for name in sorted(files):
            if name not in ("Cargo.toml", "Cargo.lock"):
                continue
            file = Path(directory) / name
            rel = file.relative_to(root).as_posix()
            with open(file, "rb") as handle:
                parsed = tomllib.load(handle)
            if name == "Cargo.toml":
                pins.extend(manifest_pins(rel, parsed))
            else:
                pins.extend(lock_pins(rel, parsed))
    return pins


def lagging_entry(path: str, lagging: dict[str, tuple[str, str]]) -> str | None:
    """The `lagging` directory `path` sits in, if any."""
    for directory in lagging:
        if path == directory or path.startswith(directory + "/"):
            return directory
    return None


def check(
    pins: list[Pin], lagging: dict[str, tuple[str, str]] | None = None
) -> tuple[str | None, list[str]]:
    """The one revision the pins agree on, and the problems found (empty when
    the pin is consistent). `lagging` defaults to `LAGGING`."""
    lagging = LAGGING if lagging is None else lagging
    problems = []
    by_rev: dict[str, list[Pin]] = {}
    held: dict[str, set[str]] = {}
    for pin in pins:
        if pin.rev is None:
            problems.append(f"{pin.path}: {pin.what} has no `rev`")
            continue
        if pin.mismatch is not None:
            problems.append(
                f"{pin.path}: {pin.what} asks for {pin.rev} but resolved "
                f"{pin.mismatch}: the lockfile is stale"
            )
        directory = lagging_entry(pin.path, lagging)
        if directory is None:
            by_rev.setdefault(pin.rev, []).append(pin)
        else:
            held.setdefault(directory, set()).add(pin.rev)
            expected = lagging[directory][0]
            if pin.rev != expected:
                problems.append(
                    f"{pin.path}: {pin.what} is at {pin.rev}, but LAGGING holds "
                    f"{directory} at {expected}"
                )
    if len(by_rev) > 1:
        problems.append("the xui revisions disagree:")
        for rev, at in sorted(by_rev.items(), key=lambda item: -len(item[1])):
            problems.append(f"  {rev}:")
            problems.extend(f"    {pin.path}: {pin.what}" for pin in at)
    rev = next(iter(by_rev)) if len(by_rev) == 1 else None
    for directory, revs in sorted(held.items()):
        if rev is not None and revs == {rev}:
            problems.append(
                f"{directory} is at the common revision now: remove it from LAGGING "
                "in tools/xui/check_pin.py"
            )
    return rev, problems


def main(argv: list[str] | None = None) -> int:
    """Check the repository; return the exit status."""
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--root", type=Path, default=ROOT, help="repository root (default: this checkout)")
    args = parser.parse_args(argv)

    pins = find_pins(args.root)
    rev, problems = check(pins)
    if problems:
        print("xui pin check failed:")
        for problem in problems:
            print(f"  {problem}")
        return 1
    files = len({pin.path for pin in pins})
    print(f"xui pin: {rev} ({len(pins)} pins in {files} files)")
    for directory, (held, reason) in sorted(LAGGING.items()):
        print(f"  {directory} held at {held}: {reason}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
