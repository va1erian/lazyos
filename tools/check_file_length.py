#!/usr/bin/env python3
"""Fail when a tracked .rs/.py file is over 500 lines (AGENTS.md, issue #484).

Files whose first lines carry an ``@generated`` marker are exempt. ``ALLOWLIST``
holds the hand-written files that are still being split, each with its current
length as a ceiling: they may shrink but never grow. Remove an entry when its
file is split (the check also complains about entries that no longer need one).
"""

from __future__ import annotations

import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
LIMIT = 500
SUFFIXES = (".rs", ".py")
HEADER_LINES = 5

# path -> maximum line count while it waits to be split.
ALLOWLIST: dict[str, int] = {
    "build.rs": 503,
    "build_support/os_image.rs": 504,
    "fuzz/gen_corpus.py": 777,
    "kernel/src/display.rs": 518,
    "kernel/src/tests/linux_suite/inet_calls.rs": 603,
    "kernel/src/tests/linux_suite/inet_core.rs": 574,
    "libs/fused/src/tests.rs": 501,
    "libs/generated/tests/display.rs": 508,
    "libs/netstack/src/stack/sockets.rs": 507,
    "libs/nvme/src/tests/model.rs": 509,
    "libs/virtio/src/queue.rs": 545,
    "tools/net/test_analyze_pcap.py": 501,
    "tools/net/test_sockets_pcap.py": 505,
    "tools/run_demo.py": 511,
    "tools/screenshot/qemu_qmp.py": 551,
    "user/src/messenger/endpoint.rs": 501,
    "xui-app/crates/archiver/src/commands.rs": 508,
    "xui-app/crates/explorer/tests/headless.rs": 507,
    "xui-app/src/backend.rs": 505,
}


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
