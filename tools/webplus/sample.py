#!/usr/bin/env python3
"""A sampling profiler for a running QEMU guest, from the host.

The guest is one CPU, so where it is at an instant is where its time goes. QEMU's monitor answers
`info registers` for the stopped vCPU; this takes thirty of them a second over a second QMP port
and keeps `CPL`, `CR3` and `RIP` of each. `CR3` names the process (one page table each), `CPL` 0 is the
kernel (its idle loop included), `RIP` is symbolized afterwards against the program's unstripped
file (`/build/webkit-port-build/bin/NetworkProcess` in the build volume).

    # a second QMP port beside the session's own
    python tools/screenshot/qemu_session.py --image target/lazyos.img --script S.json \\
        --extra-arg=-qmp --extra-arg=tcp:127.0.0.1:5555,server=on,wait=off
    python tools/webplus/sample.py record --port 5555 --out samples.tsv --seconds 300
    python tools/webplus/sample.py report samples.tsv
    python tools/webplus/sample.py report samples.tsv --cr3 14b10000 --elf NetworkProcess

With `record --stacks` a user-mode sample also keeps the `STACK_WORDS` quadwords at `RSP` (a fifth column;
empty when the guest moved on before they were read, see `snapshot`): the return addresses among them say
who called a hot leaf such as `memcpy` (`cpu_profile.py --callers`).

A static-PIE program (the WebKit processes) runs at its link addresses (the kernel moves only
dynamically linked PIEs, to 0x400000), so `--base` defaults to 0; pass `--base 0x400000` for a
dynamic one. `--elf` needs binutils' `addr2line`: the build container has it (image
`lazyos-jsc-build:1`, the unstripped programs are in the volume at
`/build/webkit-port-build/bin/`), the host usually does not.
"""

from __future__ import annotations

import argparse
import collections
import random
import re
import shutil
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools" / "screenshot"))

#: Quadwords read at `RSP` for a user-mode sample: enough for a leaf's return address and a frame or two.
STACK_WORDS = 64
#: Failed samples in a row after which the recording stops.
MAX_FAILURES = 20

# Static PIEs run unrelocated; `--base 0x400000` is for a dynamically linked PIE (the kernel's PIE_BASE).
DEFAULT_BASE = 0
_FIELDS = {
    "rip": re.compile(r"RIP=([0-9a-fA-F]{16})"),
    "cr3": re.compile(r"CR3=([0-9a-fA-F]{16})"),
    "cpl": re.compile(r"CPL=(\d)"),
}


def parse_registers(text: str) -> tuple[int, int, int] | None:
    """`(cpl, cr3, rip)` from the monitor's `info registers`, None when a field is missing."""
    found = {name: pattern.search(text) for name, pattern in _FIELDS.items()}
    if not all(found.values()):
        return None
    return int(found["cpl"].group(1)), int(found["cr3"].group(1), 16), int(found["rip"].group(1), 16)


def parse_words(text: str) -> list[int]:
    """The quadwords of the monitor's `x /Ngx` answer (`addr: 0x... 0x...` lines)."""
    words = []
    for line in text.splitlines():
        _, _, values = line.partition(":")
        words += [int(value, 16) for value in values.split() if value.startswith("0x")]
    return words


RSP = re.compile(r"RSP=([0-9a-fA-F]{16})")


def registers(qmp) -> tuple[tuple[int, int, int] | None, int | None]:
    """`parse_registers` of one `info registers`, and `RSP`."""
    text = str(qmp.execute("human-monitor-command", **{"command-line": "info registers"}))
    rsp = RSP.search(text)
    return parse_registers(text), int(rsp.group(1), 16) if rsp else None


def snapshot(qmp, stacks: bool) -> tuple[tuple[int, int, int] | None, list[int] | None]:
    """One sample: the registers, and with `stacks` the user stack's quadwords (an empty list when they
    could not be trusted). The guest runs between two monitor commands, and stopping it would refuse a
    session's input meanwhile, so the registers are read again after the stack: the stack is kept only
    when the same process is still in user mode. Its `RSP` may have moved by then (a monitor round trip
    is a millisecond of guest time), so a few of its words can be newer than the sample: `--callers`
    reads counts over many samples, not one stack. The sample is the first read either way."""
    parsed, rsp = registers(qmp)
    if not stacks or not parsed or parsed[0] != 3 or rsp is None:
        return parsed, None
    try:
        words = parse_words(str(qmp.execute("human-monitor-command", **{"command-line": f"x /{STACK_WORDS}gx {rsp:#x}"})))
    except RuntimeError:  # an unmapped stack page
        return parsed, []
    again, rsp_again = registers(qmp)
    same = again is not None and again[:2] == parsed[:2] and rsp_again is not None
    return parsed, words if same else []


def record(port: int, out: Path, seconds: float, interval: float = 0.03, stacks: bool = False) -> int:
    """Samples until `seconds` pass or QEMU goes away; writes `t<TAB>cpl<TAB>cr3<TAB>rip[<TAB>stack]` lines,
    with `stacks` the user-mode stack as comma-separated hex quadwords from `RSP` up ([`snapshot`])."""
    from qemu_qmp import Qmp  # noqa: E402  (tools/screenshot, put on the path above)

    deadline = time.time() + seconds
    qmp = None
    while qmp is None and time.time() < deadline:
        try:
            qmp = Qmp("127.0.0.1", port, 5.0)
        except (OSError, RuntimeError):  # Qmp retries for a few seconds itself, then raises RuntimeError
            time.sleep(0.5)
    if qmp is None:
        raise SystemExit(f"no QMP on port {port}")
    start = time.time()
    count = failures = 0
    with out.open("w", encoding="utf-8") as handle:
        while time.time() < deadline:
            try:
                parsed, words = snapshot(qmp, stacks)
                failures = 0
            except (OSError, EOFError, ConnectionError) as error:  # the guest has quit
                print(f"sample: QEMU went away after {count} samples: {error!r}", file=sys.stderr)
                break
            except Exception as error:  # a refused or garbled answer
                failures += 1
                print(f"sample: {error!r}", file=sys.stderr)
                if failures >= MAX_FAILURES:
                    print(f"sample: giving up after {count} samples", file=sys.stderr)
                    break
                continue
            if parsed:
                cpl, cr3, rip = parsed
                stack = "" if words is None else "\t" + ",".join(f"{word:x}" for word in words)
                handle.write(f"{time.time() - start:.3f}\t{cpl}\t{cr3:016x}\t{rip:016x}{stack}\n")
                handle.flush()
                count += 1
            # Jittered: a fixed period would alias with the 100 Hz tick and the browser's paint pacing.
            time.sleep(interval * random.uniform(0.5, 1.5))
    return count


def load(path: Path) -> list[tuple[float, int, int, int]]:
    return [row[:4] for row in load_stacks(path)]


def load_stacks(path: Path) -> list[tuple[float, int, int, int, list[int]]]:
    """`load` with each sample's stack quadwords (empty for a kernel sample or an older recording)."""
    rows = []
    for line in path.read_text(encoding="utf-8").splitlines():
        parts = line.split("\t")
        if len(parts) in (4, 5):
            stack = [int(word, 16) for word in parts[4].split(",") if word] if len(parts) == 5 else []
            rows.append((float(parts[0]), int(parts[1]), int(parts[2], 16), int(parts[3], 16), stack))
    return rows


def summarize(rows, cr3: int | None = None, top: int = 12) -> list[str]:
    """Lines of text: time by CR3 and mode, the busiest kernel and user addresses (of one `cr3`)."""
    if not rows:
        return ["no samples"]
    total = len(rows)
    lines = [f"{total} samples over {rows[-1][0]:.1f} s"]
    by_space = collections.Counter((cpl, space) for _, cpl, space, _ in rows)
    for (cpl, space), n in by_space.most_common(top):
        lines.append(f"  cpl={cpl} cr3={space:x}: {n} ({100 * n / total:.0f}%)")
    chosen = [r for r in rows if cr3 is None or r[2] == cr3]
    for cpl, name in ((0, "kernel"), (3, "user")):
        counts = collections.Counter(rip for _, c, _, rip in chosen if c == cpl)
        if counts:
            lines.append(f"{name} addresses{'' if cr3 is None else f' of cr3 {cr3:x}'}:")
            for rip, n in counts.most_common(top):
                lines.append(f"  {rip:016x}  {n}")
    return lines


def symbolize(elf: Path, addresses: list[int], base: int, tool: str | None = None,
              run=subprocess.run) -> dict[int, str]:
    """`address -> function (file:line)` through addr2line; empty when it is not installed.

    The address handed to addr2line is `address - base`. `tool` and `run` are for tests."""
    tool = tool or shutil.which("addr2line") or shutil.which("llvm-addr2line")
    if not tool or not addresses:
        return {}
    result = run(
        [tool, "-f", "-C", "-e", str(elf), *[hex(a - base) for a in addresses]],
        capture_output=True, text=True, check=False)
    rows = result.stdout.splitlines()
    return {addr: f"{rows[2 * i]} ({rows[2 * i + 1]})" for i, addr in enumerate(addresses) if 2 * i + 1 < len(rows)}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command", required=True)
    rec = sub.add_parser("record", help="sample a guest over a QMP port")
    rec.add_argument("--port", type=int, required=True)
    rec.add_argument("--out", type=Path, required=True)
    rec.add_argument("--seconds", type=float, default=300.0)
    rec.add_argument("--stacks", action="store_true", help="keep each user-mode sample's stack")
    rep = sub.add_parser("report", help="summarize a recording")
    rep.add_argument("samples", type=Path)
    rep.add_argument("--cr3", help="hex page-table base of one process")
    rep.add_argument("--elf", type=Path, help="that process's unstripped program, to name functions")
    rep.add_argument("--base", type=lambda text: int(text, 0), default=DEFAULT_BASE)
    rep.add_argument("--top", type=int, default=15)
    args = parser.parse_args()
    if args.command == "record":
        print(f"samples {record(args.port, args.out, args.seconds, stacks=args.stacks)}")
        return 0
    rows = load(args.samples)
    cr3 = int(args.cr3, 16) if args.cr3 else None
    for line in summarize(rows, cr3, args.top):
        print(line)
    if args.elf:
        if cr3 is None:
            print("--elf needs --cr3: the addresses are those of one process", file=sys.stderr)
            return 2
        users = collections.Counter(rip for _, cpl, space, rip in rows if cpl == 3 and space == cr3)
        names = symbolize(args.elf, list(users), args.base)
        if not names:
            print("no names: addr2line is not installed here (see the docstring)", file=sys.stderr)
        by_function = collections.Counter()
        for rip, n in users.items():
            by_function[names.get(rip, "?").split(" (")[0]] += n
        print("by function:")
        for name, n in by_function.most_common(args.top):
            print(f"  {n:5d}  {name}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
