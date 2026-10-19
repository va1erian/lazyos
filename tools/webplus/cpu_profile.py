#!/usr/bin/env python3
"""Where a guest program's CPU goes, by function: `sample.py`'s recording against an unstripped program.

The host has no `addr2line`, so the build container (the `jsc` Docker image and its volume, which keeps the
unstripped programs in `/build/webkit-port-build/bin/`) names the functions.

    python tools/webplus/cpu_profile.py shots/webplus-live/samples.tsv                    # which process, then its functions
    python tools/webplus/cpu_profile.py shots/webplus-live/samples.tsv --program WebProcess
    python tools/webplus/cpu_profile.py samples.tsv --program lazywebplus-app --cr3 3a4b000 --volume lazyos-jsc-scroll
    python tools/webplus/cpu_profile.py samples.tsv --program lazywebplus-app --callers memcpy   # who calls the hot leaf

`--callers` reads the stack quadwords `sample.py` keeps with a user-mode sample and names, for each sample
in a function matching the text, the first quadword that symbolizes to another function: a return address
in the common case (a heuristic: a stale pointer into the program can be named instead, so read the counts,
not a single sample).

The process is the user-mode address space with the most samples unless `--cr3` says which (`sample.py report`
lists them). A static-PIE program runs at its link addresses, so no base is subtracted.
"""

from __future__ import annotations

import argparse
import collections
import os
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import sample  # noqa: E402

IMAGE = "lazyos-jsc-build:1"
BIN = "/build/webkit-port-build/bin"
#: Addresses of the kernel half of the address space: not in the program.
KERNEL = 0xFFFF_8000_0000_0000


def container_addr2line(volume: str, program: str):
    """A `subprocess.run` stand-in that runs addr2line on `program` inside the build container."""

    def run(command, **kwargs):
        # command: [tool, "-f", "-C", "-e", elf, addr...]
        addresses = command[5:]
        script = f"addr2line -f -C -e {BIN}/{program} " + " ".join(addresses)
        return subprocess.run(["docker", "run", "--rm", "--entrypoint", "sh", "-v", f"{volume}:/build:ro", IMAGE, "-c", script],
                              capture_output=True, text=True, check=False)

    return run


def profile(rows, program: str, volume: str, cr3: int | None, top: int, run=None) -> list[str]:
    user = [r for r in rows if r[1] == 3]
    total = len(rows)
    lines = [f"{total} samples, {len(user)} in user mode"]
    by_space = collections.Counter(r[2] for r in user)
    for space, n in by_space.most_common(5):
        lines.append(f"  user cr3={space:x}: {n} samples ({100 * n / total:.0f}% of all)")
    chosen = cr3 if cr3 is not None else (by_space.most_common(1)[0][0] if by_space else None)
    if chosen is None:
        return lines + ["no user-mode samples"]
    addresses = collections.Counter(r[3] for r in user if r[2] == chosen)
    lines.append(f"process cr3={chosen:x}: {sum(addresses.values())} samples over {len(addresses)} addresses; {program}:")
    names = sample.symbolize(Path(program), list(addresses), 0, tool="addr2line",
                             run=run or container_addr2line(volume, program))
    by_function: collections.Counter = collections.Counter()
    for address, count in addresses.items():
        label = names.get(address, "??")
        function = label.rsplit(" (", 1)[0]
        by_function[function] += count
    covered = sum(by_function.values())
    for function, count in by_function.most_common(top):
        lines.append(f"  {100 * count / covered:5.1f}%  {count:5d}  {function[:150]}")
    return lines


#: A quadword further from zero than this is not an address in a static program's text.
TEXT_LIMIT = 0x4000_0000


def callers(rows, program: str, volume: str, cr3: int, leaf: str, top: int, run=None) -> list[str]:
    """Who called the functions matching `leaf`, by the first other function among the stack words."""
    run = run or container_addr2line(volume, program)
    chosen = [r for r in rows if r[1] == 3 and r[2] == cr3]
    words = {w for r in chosen for w in r[4] if 0 < w < TEXT_LIMIT} | {r[3] for r in chosen}
    names = sample.symbolize(Path(program), sorted(words), 0, tool="addr2line", run=run)

    def name(address: int) -> str:
        return names.get(address, "??").rsplit(" (", 1)[0]

    hits = [r for r in chosen if leaf in name(r[3])]
    by_caller: collections.Counter = collections.Counter()
    for r in hits:
        own = name(r[3])
        caller = next((name(w) for w in r[4] if w in names and name(w) not in ("??", own) and leaf not in name(w)),
                      "(no other function on the stack)")
        by_caller[caller] += 1
    lines = [f"{len(hits)} samples in functions matching {leaf!r}; first other function on the stack:"]
    for caller, count in by_caller.most_common(top):
        lines.append(f"  {count:5d}  {caller[:150]}")
    return lines


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("samples", type=Path)
    parser.add_argument("--program", default="WebProcess", help="the unstripped program in the build volume")
    parser.add_argument("--volume", default=os.environ.get("LAZYOS_JSC_VOLUME", "lazyos-jsc-work"))
    parser.add_argument("--cr3", help="hex page-table base of the process (default: the busiest user space)")
    parser.add_argument("--top", type=int, default=30)
    parser.add_argument("--callers", metavar="TEXT", help="name the callers of the functions matching TEXT")
    args = parser.parse_args()
    cr3 = int(args.cr3, 16) if args.cr3 else None
    if args.callers:
        rows = sample.load_stacks(args.samples)
        if cr3 is None:
            busiest = collections.Counter(r[2] for r in rows if r[1] == 3).most_common(1)
            cr3 = busiest[0][0] if busiest else 0
        print("\n".join(callers(rows, args.program, args.volume, cr3, args.callers, args.top)))
        return 0
    rows = sample.load(args.samples)
    print("\n".join(profile(rows, args.program, args.volume, cr3, args.top)))
    return 0


if __name__ == "__main__":
    sys.exit(main())
