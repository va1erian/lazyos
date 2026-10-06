#!/usr/bin/env python3
"""Host-side audit of the OS volume: what changed between two images?

`osread IMAGE tree /` (libs/ext2fs/examples/osread.rs) lists every node with
its mode, owner, size, mtime and a content hash. `diff` compares two such
listings and `judge` decides which differences an attack session may have
left: none outside `ALLOWED` (the user's home, the journals and scratch that
every boot rewrites), except what a still-open (`xfail`) scenario that
SUCCEEDED is declared to touch (`attack_judge.Expect.touches`). Once a
scenario is `blocked`, nothing excuses its paths and any change is a failure.

    python tools/accounts/audit.py before.txt after.txt
"""

from __future__ import annotations

import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent

#: Paths the session user may change, and what every boot rewrites on its own
#: (the journals; `lost+found`). Everything else is the system's.
ALLOWED = ("/home/user", "/logs", "/lost+found", "/tmp", "/transient",
           # The Terminal's shell history while the session still runs as root
           # (`admin`, before U0, #623); the session's home is /home/user after.
           "/home/admin/.ash_history")

# kind, mode, uid, gid, size, mtime, hash
Node = tuple[str, ...]


def read_tree(image: Path) -> str:
    """The `tree` listing of `image`'s OS volume (empty text on failure)."""
    result = subprocess.run(["cargo", "run", "-q", "-p", "ext2fs", "--example", "osread", "--",
                             str(image), "tree", "/"],
                            cwd=ROOT, capture_output=True, text=True, errors="replace")
    if result.returncode != 0:
        print(result.stderr[-2000:], file=sys.stderr)
        return ""
    return result.stdout


def parse(text: str) -> dict[str, Node]:
    """path -> (kind, mode, uid, gid, size, mtime, hash). A directory's size and
    mtime move whenever an entry does, so only its owner and mode count."""
    nodes: dict[str, Node] = {}
    for line in text.splitlines():
        fields = line.split(" ", 7)
        if len(fields) != 8:
            continue
        kind, mode, uid, gid, size, mtime, digest, path = fields
        nodes[path] = (kind, mode, uid, gid, "-", "-", "-") if kind == "d" else \
            (kind, mode, uid, gid, size, mtime, digest)
    return nodes


def diff(before: dict[str, Node], after: dict[str, Node]) -> list[tuple[str, str]]:
    """(path, description) of every added, removed or changed path, sorted."""
    changes = [(path, "removed") for path in before.keys() - after.keys()]
    changes += [(path, "added") for path in after.keys() - before.keys()]
    for path in before.keys() & after.keys():
        if before[path] != after[path]:
            changes.append((path, f"changed {' '.join(before[path])} -> {' '.join(after[path])}"))
    return sorted(changes)


def under(path: str, prefixes: tuple[str, ...] | list[str]) -> bool:
    return any(path == prefix or path.startswith(prefix.rstrip("/") + "/") for prefix in prefixes)


def judge(changes: list[tuple[str, str]], excused: list[str], allowed: tuple[str, ...] = ALLOWED
          ) -> tuple[list[str], list[str]]:
    """(failures, notes): `excused` are the paths still-open attacks may touch."""
    failures: list[str] = []
    notes: list[str] = []
    for path, what in changes:
        if under(path, allowed):
            continue
        if under(path, excused):
            notes.append(f"audit: {path} {what} (an open attack, xfail)")
        else:
            failures.append(f"audit: {path} {what}")
    return failures, notes


def main() -> int:
    if len(sys.argv) != 3:
        print(__doc__)
        return 2
    before, after = (parse(Path(arg).read_text(encoding="utf-8")) for arg in sys.argv[1:])
    failures, notes = judge(diff(before, after), [])
    for line in notes + failures:
        print(line)
    print("PASS" if not failures else f"{len(failures)} change(s) outside the allowed paths")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
