#!/usr/bin/env python3
"""Where a guest program's CPU goes, by function: `sample.py`'s recording against an unstripped program.

The host has no `addr2line`, so the build container (the `jsc` Docker image and its volume, which keeps the
unstripped programs in `/build/webkit-port-build/bin/`) names the functions.

    python tools/webplus/cpu_profile.py shots/webplus-live/samples.tsv                    # which process, then its functions
    python tools/webplus/cpu_profile.py shots/webplus-live/samples.tsv --program WebProcess
    python tools/webplus/cpu_profile.py samples.tsv --program lazywebplus-app --cr3 3a4b000 --volume lazyos-jsc-scroll

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


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("samples", type=Path)
    parser.add_argument("--program", default="WebProcess", help="the unstripped program in the build volume")
    parser.add_argument("--volume", default=os.environ.get("LAZYOS_JSC_VOLUME", "lazyos-jsc-work"))
    parser.add_argument("--cr3", help="hex page-table base of the process (default: the busiest user space)")
    parser.add_argument("--top", type=int, default=30)
    args = parser.parse_args()
    rows = sample.load(args.samples)
    cr3 = int(args.cr3, 16) if args.cr3 else None
    print("\n".join(profile(rows, args.program, args.volume, cr3, args.top)))
    return 0


if __name__ == "__main__":
    sys.exit(main())
