#!/usr/bin/env python3
"""Fail when a tracked .rs/.py file is over 500 lines (AGENTS.md, issue #484).

Files whose first lines carry an ``@generated`` marker are exempt. Every
hand-written file is now under the limit, so ``ALLOWLIST`` is empty; it stays
as the escape hatch for a file mid-split, with its current length as a ceiling
(it may shrink but never grow). The check complains about entries that no
longer need one.
"""

from __future__ import annotations

import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
LIMIT = 500
SUFFIXES = (".rs", ".py")
HEADER_LINES = 5

# path -> maximum line count while it waits to be split (empty: keep it so).
ALLOWLIST: dict[str, int] = {}


def tracked_files(root: Path = ROOT) -> list[str]:
    out = subprocess.run(
        ["git", "ls-files", "-z"], cwd=root, check=True, capture_output=True
    ).stdout.decode("utf-8")
    return [p for p in out.split("\0") if p.endswith(SUFFIXES)]


def is_generated(lines: list[str]) -> bool:
    return any("@generated" in line for line in lines[:HEADER_LINES])


def check(paths: list[str], root: Path = ROOT, allow: dict[str, int] | None = None) -> list[str]:
    """Return one message per violation."""
    allow = ALLOWLIST if allow is None else allow
    problems: list[str] = []
    seen: set[str] = set()
    for rel in paths:
        path = root / rel
        if not path.is_file():
            continue
        lines = path.read_text(encoding="utf-8", errors="replace").splitlines()
        if is_generated(lines):
            continue
        n = len(lines)
        if rel in allow:
            seen.add(rel)
            if n > allow[rel]:
                problems.append(f"{rel}: {n} lines, grew past its allowlisted {allow[rel]}")
            elif n <= LIMIT:
                problems.append(f"{rel}: {n} lines, drop it from ALLOWLIST")
        elif n > LIMIT:
            problems.append(f"{rel}: {n} lines (limit {LIMIT}); split it by responsibility")
    for rel in allow:
        if rel not in seen and rel not in paths:
            problems.append(f"{rel}: allowlisted but not a tracked file")
    return problems


def main() -> int:
    problems = check(tracked_files())
    for p in problems:
        print(p, file=sys.stderr)
    if problems:
        return 1
    print(f"file length ok (limit {LIMIT}, {len(ALLOWLIST)} allowlisted)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
