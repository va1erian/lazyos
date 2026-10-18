#!/usr/bin/env python3
"""Ratchet the raw `spin` locks: per-file counts may only go down.

docs/smp-plan.md section 4.3. SMP-touched critical sections use the kernel's
`sync::Lock` (lockdep, owner tracking, spin watchdog); the raw `spin::Mutex` /
`spin::RwLock` uses are today's single-CPU locks, migrated subsystem by
subsystem (S8.1+). This lint is the progress bar of that migration: a new file
with a raw lock fails, a file whose count rises fails, and a file that
improves must lower its entry (`--update-baseline`), so saved headroom cannot
be spent on a new raw lock.

A hit is a `spin` lock type use:

    use spin::Mutex;                 static T: Mutex<[..]>  = ..;
    static T: spin::Mutex<..>;       let l = spin::mutex::Mutex::new(..);
    static R: RwLock<..>;            use spin::RwLock;

Not hits: `sync::Lock` (the kernel lock type, once it exists), `YieldMutex<T>`
uses (the alias itself is in the baseline; its S8.6 migration lowers it), the
word "Mutex" in comments or strings without a generic or path use, and every
file under `vendor/` (upstream code this repo does not edit).
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
BASELINE = Path(__file__).with_name("lock_baseline.json")

# A raw spin lock use: the fully-qualified path (`spin::Mutex`,
# `spin::mutex::Mutex`, `spin::RwLock`) or a short generic use (`Mutex<`,
# `RwLock<`) of a name a `use spin::..` brought in.
LOCK = re.compile(r"\bspin::(?:mutex::)?(?:Mutex|RwLock)\b|\b(?:Mutex|RwLock)\s*<")

# Not scanned: upstream code this repo does not edit.
EXCLUDED_PREFIXES = ("vendor/",)


def tracked_rust_files(root: Path) -> list[str]:
    """The repo's `.rs` files, relative paths. Outside a git repo (a test's
    temp tree) the directory is walked instead."""
    out = subprocess.run(
        ["git", "ls-files", "--cached", "--others", "--exclude-standard", "*.rs"],
        cwd=root, capture_output=True, text=True,
    )
    if out.returncode == 0:
        return [line for line in out.stdout.splitlines() if line]
    return [
        path.relative_to(root).as_posix()
        for path in sorted(root.rglob("*.rs"))
        if ".git" not in path.parts and "/target/" not in path.as_posix()
    ]


def scan_text(text: str) -> list[int]:
    """Lines of one file's text with a raw lock use."""
    return [text.count("\n", 0, match.start()) + 1 for match in LOCK.finditer(text)]


def scan(root: Path) -> dict[str, list[int]]:
    result: dict[str, list[int]] = {}
    for path in tracked_rust_files(root):
        if path.startswith(EXCLUDED_PREFIXES) or "/target/" in path:
            continue
        file = root / path
        if not file.is_file():
            continue
        text = file.read_text(encoding="utf-8", errors="replace")
        hits = scan_text(text)
        if hits:
            result[path] = hits
    return result


def ratchet(found: dict[str, list], baseline: dict[str, int]) -> list[str]:
    """What is wrong with the counts against the saved baseline: a file with
    more hits than it had (or any hit in a file with none), and a file with
    fewer that left its entry too high."""
    problems = []
    for path in sorted(set(found) | set(baseline)):
        now, was = len(found.get(path, [])), baseline.get(path, 0)
        if now > was:
            problems.append(
                f"{path}: {now} raw spin lock(s), baseline {was}: SMP-era critical "
                f"sections use `sync::Lock` (docs/smp-plan.md section 4)"
            )
        elif now < was:
            problems.append(
                f"{path}: {now} raw spin lock(s), baseline {was}: lower it with "
                f"`python tools/sync/check_locks.py --update-baseline`"
            )
    return problems


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument(
        "--ratchet",
        action="store_true",
        help=f"fail when a file has more raw locks than {BASELINE.name} (or fewer: lower it)",
    )
    parser.add_argument(
        "--update-baseline", action="store_true", help=f"rewrite {BASELINE.name} from the tree"
    )
    parser.add_argument("--baseline", type=Path, default=BASELINE, help=argparse.SUPPRESS)
    parser.add_argument("--root", type=Path, default=ROOT)
    parser.add_argument("--verbose", "-v", action="store_true", help="list every hit line")
    args = parser.parse_args(argv)

    found = scan(args.root)
    for path in sorted(found, key=lambda p: (-len(found[p]), p)):
        print(f"{len(found[path]):4d}  {path}")
        if args.verbose:
            for line in found[path]:
                print(f"        {line}: raw spin lock")
    total = sum(len(hits) for hits in found.values())
    print(f"{total:4d}  total, {len(found)} files")

    if args.update_baseline:
        counts = {path: len(found[path]) for path in sorted(found)}
        args.baseline.write_text(json.dumps(counts, indent=1) + "\n", encoding="utf-8")
        print(f"wrote {args.baseline.name}: {total} locks in {len(counts)} files")
        return 0
    if args.ratchet:
        saved = (
            json.loads(args.baseline.read_text(encoding="utf-8")) if args.baseline.is_file() else {}
        )
        problems = ratchet(found, saved)
        for problem in problems:
            print(f"RATCHET  {problem}", file=sys.stderr)
        if problems:
            print("check_locks: SMP-era locks go through `sync::Lock` (docs/smp-plan.md)", file=sys.stderr)
            return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
