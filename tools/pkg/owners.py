#!/usr/bin/env python3
"""Check that every file of the image belongs to exactly one system package.

The image build writes `target/os-owners.tsv` (`path<TAB>producer`, see
`build_support/os_owners.rs`); `packaging/packages.toml` lists the system
packages (docs/update-plan.md decisions 16-17). This joins the two:

    python tools/pkg/owners.py [--table target/os-owners.tsv] [--packages packaging/packages.toml]

A file is claimed by the packages whose `paths` match it. When none does, by
the package whose `producers` names the module that placed it. Zero claims
(an unowned file) or several at the same level (an ambiguous one) fail, and so
does a list that is malformed or gives one producer to two packages. Exit
status 1 on any problem.
"""

from __future__ import annotations

import argparse
import fnmatch
import re
import sys
import tomllib
from collections import Counter
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
KINDS = {"kernel", "boot", "service", "tool", "data", "app", "meta"}
ACTIVATIONS = {"none", "next-launch", "restart-units", "restart-session", "reboot"}
VOLUMES = {"os", "boot", "esp"}
NAME = re.compile(r"^[a-z][a-z0-9.-]*$")


def load(path: Path) -> tuple[list[dict], list[str], list[str]]:
    """The packages, the seed paths, and every problem with the list itself."""
    data = tomllib.loads(path.read_text(encoding="utf-8"))
    packages = data.get("package", [])
    seeds = data.get("seeds", {}).get("paths", [])
    problems: list[str] = []
    names: set[str] = set()
    producers: dict[str, str] = {}
    for package in packages:
        name = package.get("name", "<unnamed>")
        if not NAME.match(name):
            problems.append(f"{name}: bad package name")
        if name in names:
            problems.append(f"{name}: listed twice")
        names.add(name)
        if package.get("kind") not in KINDS:
            problems.append(f"{name}: kind must be one of {sorted(KINDS)}")
        if package.get("activation") not in ACTIVATIONS:
            problems.append(f"{name}: activation must be one of {sorted(ACTIVATIONS)}")
        volume = package.get("volume", "os")
        if volume not in VOLUMES:
            problems.append(f"{name}: volume must be one of {sorted(VOLUMES)}")
        if volume == "os" and not (package.get("paths") or package.get("producers")):
            problems.append(f"{name}: an OS-volume package needs paths or producers")
        for pattern in package.get("paths", []):
            if not pattern.startswith("/"):
                problems.append(f"{name}: path `{pattern}` is not absolute")
        for producer in package.get("producers", []):
            if producer in producers:
                problems.append(f"producer {producer} is given to both {producers[producer]} and {name}")
            producers[producer] = name
    return packages, seeds, problems


def matches(pattern: str, path: str) -> bool:
    return path == pattern if "*" not in pattern else fnmatch.fnmatchcase(path, pattern)


def owner_of(path: str, producer: str, packages: list[dict]) -> tuple[list[str], str]:
    """The packages claiming `path`, and how: `paths` or `producers`."""
    by_path = [p["name"] for p in packages if any(matches(x, path) for x in p.get("paths", []))]
    if by_path:
        return by_path, "paths"
    return [p["name"] for p in packages if producer in p.get("producers", [])], "producers"


def check(table: str, packages: list[dict], seeds: list[str]) -> tuple[Counter, list[str]]:
    """Owner counts per package and the problems found in the table."""
    counts: Counter = Counter()
    problems: list[str] = []
    for number, line in enumerate(table.splitlines(), 1):
        path, _, producer = line.partition("\t")
        if not path or not producer:
            problems.append(f"owners table line {number} is not `path<TAB>producer`")
            continue
        if any(matches(seed, path) for seed in seeds):
            counts["(seed)"] += 1
            continue
        owners, how = owner_of(path, producer, packages)
        if not owners:
            problems.append(f"unowned: {path} (placed by {producer})")
        elif len(owners) > 1:
            problems.append(f"ambiguous: {path} is claimed by {', '.join(owners)} through {how}")
        else:
            counts[owners[0]] += 1
    return counts, problems


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--table", type=Path, default=ROOT / "target" / "os-owners.tsv")
    ap.add_argument("--packages", type=Path, default=ROOT / "packaging" / "packages.toml")
    args = ap.parse_args()
    packages, seeds, problems = load(args.packages)
    if not args.table.is_file():
        sys.exit(f"{args.table} not found: build an image first (cargo build writes it)")
    counts, found = check(args.table.read_text(encoding="utf-8"), packages, seeds)
    problems += found
    for problem in problems:
        print(problem)
    files = sum(counts.values())
    print(f"{files} files in {len(counts)} packages ({len(packages)} listed), {len(problems)} problem(s)")
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
