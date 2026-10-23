#!/usr/bin/env python3
"""More allocator slots than CPUs: a patch to the zig toolchain the programs are linked with.

Every program of the port gets its C library from zig, and zig's `malloc` is `std.heap.SmpAllocator`:
one slot (a free list behind a spin mutex) per CPU, picked by `index = (index + 1) % cpu_count` and
taken with `tryLock` in a loop that never yields. The guest reports one CPU (`sched_getaffinity`), so
there is one slot. On one CPU a thread that is preempted inside `malloc` still holds it, and every
other thread that allocates then spins for its whole time slice until the holder runs again: a
sample of the WebProcess scrolling a real page put 17% of its user time in `Thread.lock`, and the
spinning threads kept the main thread, which answers the wheel, waiting (docs/lazyweb-performance-plan.md,
"Scroll latency"). With `MIN_SLOTS` slots a thread that finds its slot held takes the next one, and a
preempted holder blocks only its own. Slots are interchangeable (any thread may free into any), so
this costs a little fragmentation and nothing else.

`apply()` edits the toolchain in place (in the container, the pip wheel of the image; with `--no-docker` on a host
it edits that host's installed `ziglang`, permanently, which is what that flag asks for) and is idempotent: the container is new for every build, so
`port_cmake.build` calls it each time. A program linked before the fix keeps the old allocator
(ninja cannot see a change inside the toolchain), so `stale()` names the ones to link again
(`VERSION` is recorded per program in the work volume).
"""

from __future__ import annotations

import json
from pathlib import Path

from toolenv import BuildError

#: Slots the allocator has at least, whatever `std.Thread.getCpuCount` says (zig's maximum is 128).
MIN_SLOTS = 16
#: Bump when the patch changes: every program is linked again.
VERSION = "1"
ANCHOR = "const n: u32 = @min(std.Thread.getCpuCount() catch max_thread_count, max_thread_count);"
FIXED = (f"const n: u32 = @min(@max(std.Thread.getCpuCount() catch max_thread_count, {MIN_SLOTS}), "
         "max_thread_count);")
RELATIVE = Path("lib/std/heap/SmpAllocator.zig")


def patched(text: str) -> str | None:
    """`text` with the fix, or None when it already has it; BuildError for an allocator this does not know."""
    if FIXED in text:
        return None
    if ANCHOR not in text:
        raise BuildError("zig's SmpAllocator.getCpuCount is not the one zigfix.py patches "
                         "(a new zig: look at std/heap/SmpAllocator.zig and update tools/webkit/zigfix.py)")
    return text.replace(ANCHOR, FIXED)


def toolchain_file() -> Path:
    import ziglang
    return Path(ziglang.__file__).resolve().parent / RELATIVE


def apply(path: Path | None = None) -> bool:
    """Patch the toolchain's allocator; True when this call changed it."""
    path = path or toolchain_file()
    text = path.read_text(encoding="utf-8")
    fixed = patched(text)
    if fixed is None:
        return False
    path.write_text(fixed, encoding="utf-8", newline="\n")
    return True


def stale(record: Path, names: list[str]) -> list[str]:
    """The programs among `names` not linked with this `VERSION` of the fix."""
    try:
        done = json.loads(record.read_text())
    except (OSError, ValueError):
        done = {}
    return [name for name in names if done.get(name) != VERSION]


def mark(record: Path, names: list[str]) -> None:
    try:
        done = json.loads(record.read_text())
    except (OSError, ValueError):
        done = {}
    done.update({name: VERSION for name in names})
    record.write_text(json.dumps(done, indent=1))
