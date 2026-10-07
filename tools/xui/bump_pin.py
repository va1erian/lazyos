#!/usr/bin/env python3
"""Move every xui pin to one new revision, then check it.

Bumping by hand means editing a dozen manifests, refreshing three lockfiles and
remembering `docs/xui-plan.md`. This does all of it:

    python tools/xui/bump_pin.py <rev|branch|main>     # a SHA, or what to resolve
    python tools/xui/bump_pin.py main --dry-run        # show what would change
    python tools/xui/bump_pin.py <rev> --skip lazyrad-os

It resolves the revision through GitHub (`gh api`) to a full SHA, rewrites the
`rev` of every `va1erian/xui` git dependency in every `Cargo.toml`, runs
`cargo update` for the xui packages in each workspace that has a lockfile, and
finishes with `check_pin.py`. `lazyrad-os` also pins LazyRAD (a repository with
its own xui pin): bump LazyRAD first and give its commit with `--lazyrad`,
or `--skip lazyrad-os` until it has caught up (and list it in `LAGGING` of
`check_pin.py`).
"""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent
SKIP_DIRS = {".git", ".claude", "target", "node_modules"}
# A `va1erian/xui` git dependency and its `rev`, on one line.
XUI_DEP = re.compile(r'(git\s*=\s*"https://(?:www\.)?github\.com/va1erian/xui"[^\n}]*?rev\s*=\s*")([0-9a-f]{7,40})(")')
LAZYRAD_DEP = re.compile(r'(git\s*=\s*"https://github\.com/va1erian/lazyrad"[^\n}]*?rev\s*=\s*")([0-9a-f]{7,40})(")')
# The `rev = "<sha>"` docs/xui-plan.md quotes.
DOC_REV = re.compile(r'(rev = ")([0-9a-f]{40})(")')
# The workspaces with a lockfile of their own (relative to the root).
WORKSPACES = ["xui-app", "doom", "lazyrad-os"]


def resolve(repo: str, ref: str) -> str:
    """The full commit SHA of `ref` in va1erian/`repo`."""
    if re.fullmatch(r"[0-9a-f]{40}", ref):
        return ref
    out = subprocess.run(
        ["gh", "api", f"repos/va1erian/{repo}/commits/{ref}", "-q", ".sha"],
        capture_output=True, text=True,
    )
    sha = out.stdout.strip()
    if out.returncode != 0 or not re.fullmatch(r"[0-9a-f]{40}", sha):
        sys.exit(f"cannot resolve {ref!r} in va1erian/{repo}: {out.stderr.strip()}")
    return sha


def manifests(skip: set[str]) -> list[Path]:
    found = []
    for path in sorted(ROOT.rglob("Cargo.toml")):
        parts = path.relative_to(ROOT).parts
        if SKIP_DIRS & set(parts) or parts[0] in skip:
            continue
        found.append(path)
    return found


def rewrite(paths: list[Path], pattern: re.Pattern[str], rev: str, dry: bool) -> int:
    changed = 0
    for path in paths:
        text = path.read_text(encoding="utf-8")
        new, n = pattern.subn(lambda m: m.group(1) + rev + m.group(3), text)
        if new != text:
            changed += 1
            print(f"{'would change' if dry else 'rewrote'} {path.relative_to(ROOT).as_posix()} ({n})")
            if not dry:
                path.write_bytes(new.encode("utf-8"))
    return changed


def update_locks(skip: set[str]) -> bool:
    """Refresh each lockfile. Cargo repairs the entries whose git `rev` changed
    on any command that may write the lock, touching nothing else (`cargo
    update -p` cannot name the old entries once the manifests have moved)."""
    ok = True
    for workspace in WORKSPACES:
        if workspace in skip or not (ROOT / workspace / "Cargo.lock").is_file():
            continue
        print(f"refreshing {workspace}/Cargo.lock")
        result = subprocess.run(["cargo", "metadata", "--format-version", "1"], cwd=ROOT / workspace,
                                capture_output=True, text=True, encoding="utf-8")
        if result.returncode != 0:
            print(result.stderr.strip())
            ok = False
    return ok


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("ref", help="the xui commit (SHA, branch or tag)")
    parser.add_argument("--lazyrad", metavar="REF", help="also pin this LazyRAD commit in lazyrad-os")
    parser.add_argument("--skip", action="append", default=[], metavar="DIR", help="leave this workspace alone")
    parser.add_argument("--dry-run", action="store_true", help="only list the manifests that would change")
    args = parser.parse_args(argv)
    skip = set(args.skip)

    rev = resolve("xui", args.ref)
    print(f"xui {rev}")
    changed = rewrite(manifests(skip), XUI_DEP, rev, args.dry_run)
    if args.lazyrad:
        lazyrad = resolve("lazyrad", args.lazyrad)
        print(f"lazyrad {lazyrad}")
        changed += rewrite(manifests(skip), LAZYRAD_DEP, lazyrad, args.dry_run)
    # The one place the docs name the revision in full.
    plan = ROOT / "docs" / "xui-plan.md"
    if plan.is_file():
        changed += rewrite([plan], DOC_REV, rev, args.dry_run)
    if args.dry_run:
        print(f"{changed} manifest(s) would change")
        return 0
    if not update_locks(skip):
        return 1
    check = subprocess.run([sys.executable, str(ROOT / "tools" / "xui" / "check_pin.py")], cwd=ROOT)
    return check.returncode


if __name__ == "__main__":
    raise SystemExit(main())
