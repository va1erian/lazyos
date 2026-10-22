#!/usr/bin/env python3
"""A page mapping is invalidated through `mem::tlb`, never with a bare `invlpg`.

docs/smp-plan.md S8.2 and docs/architecture/virtual-memory.md: a local
`tlb::flush` only cleans this CPU's TLB, so a stale entry on another CPU can
write through to a freed frame. Every unmap or write-protect goes through
`mem::tlb::{shootdown, shootdown_all, Batch}`. The bare calls that remain are
listed here with the reason they are safe, and this lint fails when a new one
appears or an old one moves out of its file:

* `mem/tlb.rs` is the implementation and `arch/smp/shootdown.rs` the remote
  half; `arch/smp/bringup.rs` flushes a new AP's own TLB once it is online.
* `pagefault.rs`, `filemap.rs` and `uspace.rs` each have one site that only
  *loosens* an entry (a COW page made writable in place, a shared page made
  writable, a read-only parent made writable): a remote CPU's stale
  read-only copy costs it a retried fault (`arch::spurious_fault`), never a
  wrong access (the argument is in `mem/tlb.rs`).

Run `python tools/sync/check_tlb.py`; `test_check_tlb.py` is its self-test.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
KERNEL = ROOT / "kernel" / "src"

# The bare invalidations: `tlb::flush(` / `tlb::flush_all(` on x86_64's
# instructions module (however it is imported) and a literal `invlpg` (the
# shootdown API is `tlb::shootdown`, not matched).
BARE = re.compile(r"\btlb::flush(?:_all)?\s*\(|\binvlpg\b")

# path (under kernel/src) -> how many bare calls it may hold.
ALLOWED = {
    "mem/tlb.rs": 4,  # flush_local (2), shootdown_all, Batch::note
    "arch/smp/shootdown.rs": 2,  # the shootdown handler's flush_all / flush
    "arch/smp/bringup.rs": 1,  # a new AP's full flush after it goes online
    "mem/pagefault.rs": 1,  # COW made writable in place: loosens
    "mem/filemap.rs": 1,  # shared page made writable: loosens
    "mem/uspace.rs": 1,  # make_walk_writable: loosens
}


def scan(root: Path = KERNEL) -> dict[str, int]:
    """Bare-flush counts per file, tests excluded (they stage their own)."""
    found: dict[str, int] = {}
    for path in sorted(root.rglob("*.rs")):
        rel = path.relative_to(root).as_posix()
        if rel.startswith("tests/"):
            continue
        count = sum(
            1
            for line in path.read_text(encoding="utf-8", errors="replace").splitlines()
            if BARE.search(line) and not line.lstrip().startswith("//")
        )
        if count:
            found[rel] = count
    return found


def problems(found: dict[str, int]) -> list[str]:
    out = []
    for rel, count in found.items():
        allowed = ALLOWED.get(rel, 0)
        if count > allowed:
            out.append(
                f"{rel}: {count} bare TLB flush(es), {allowed} allowed: "
                "use mem::tlb::{shootdown, shootdown_all, Batch}"
            )
    for rel, allowed in ALLOWED.items():
        if found.get(rel, 0) < allowed:
            out.append(f"{rel}: allows {allowed} but holds {found.get(rel, 0)}: lower ALLOWED")
    return out


def main() -> int:
    bad = problems(scan())
    for line in bad:
        print(f"check_tlb: {line}", file=sys.stderr)
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
