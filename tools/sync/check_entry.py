#!/usr/bin/env python3
"""Every way back to an interrupted context goes through the entry lock.

docs/architecture/smp.md, "The entry lock": a CPU holds the kernel entry
lock (`sync::entry`) while it runs kernel code; each gate's Rust callee
calls `sync::entry::enter` (or `fault_gate`) with the interrupted `CS` and
`leave` with the resumed one. The `irq_gate!`/`paranoid_gate!` macros
generate that; the naked stubs call a hand-written callee that must do it. A
stub that forgets passes every single-CPU test and runs kernel code
unserialized once the APs run tasks, so this lint pins it:

* every `iretq`/`sysretq` instruction in the kernel (outside the suite;
  comments stripped) sits in a file listed in ALLOWED, with its count;
* each naked stub file calls exactly the callees STUB_CALLS names, and each
  of those callees calls the entry hooks (HOOKED);
* every `window_safe`/`unlocked` gate (which takes no lock) is listed in
  OPT_OUTS with the reason it may run unlocked;
* every halt with interrupts on (`enable_and_hlt`) sits in `sync::entry`
  (HALTS): kernel code waits for an interrupt through `sync::entry::halt`,
  which drops the hold around the `hlt` (S8.3b). A bare one would sleep
  holding the lock and keep every other CPU out of the kernel meanwhile.

The window-safe exits (the NMI's, the trampoline's early NMI stub) take no
lock and are listed with that reason. The checks are textual: they catch a
stub or opt-out added without thought, not one written to fool them.

Run `python tools/sync/check_entry.py`; `test_check_entry.py` is its self-test.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
KERNEL = ROOT / "kernel" / "src"

# The exit instructions, as a token anywhere in comment-stripped text (an asm
# line, a `"... iretq\n"` or `" iretq"` string piece, with or without a
# trailing comment).
EXIT = re.compile(r"(?<![\w])(?:iretq|sysretq)(?![\w])")

# path (under kernel/src) -> how many exits it may hold.
ALLOWED = {
    "arch/gsgate.rs": 2,  # irq_gate!, paranoid_gate!: generated `locked` wrappers (OPT_OUTS aside)
    "arch/idt.rs": 2,  # exception_isr, page_fault_isr: STUB_CALLS
    "arch/linux.rs": 2,  # the syscall stub's sysretq and iretq: STUB_CALLS
    "process/gate.rs": 1,  # int 0x80: STUB_CALLS
    "task/switch.rs": 1,  # timer/yield: STUB_CALLS
    "arch/nmi.rs": 2,  # the NMI and a dropped nested NMI: window-safe, no lock
    "arch/smp/trampoline.rs": 1,  # an AP's early NMI stub, before it runs anything
}

# The naked stubs' files -> every Rust function their asm calls. A `call`
# not listed (or a listed one gone) fails: the stub would bypass its hooks.
STUB_CALLS = {
    "arch/idt.rs": {"exception_dispatch", "page_fault_dispatch"},
    "arch/linux.rs": {"linux_dispatch_gate", "linux_syscall_return"},
    "process/gate.rs": {"syscall_dispatch"},
    # `window_tick` is the interrupt-window tick: window-safe (it runs in a
    # syscall's window, on the holder, and takes no lock).
    "task/switch.rs": {"schedule", "window_tick"},
}

# The callees that bracket their stub's work: (file, function, where the
# matching exit is when it is another function).
HOOKED = [
    ("task/schedule.rs", "schedule", None),
    ("process/gate.rs", "syscall_dispatch", None),
    ("arch/idt.rs", "exception_dispatch", None),
    ("arch/idt.rs", "page_fault_dispatch", None),
    ("process/linux/mod.rs", "linux_dispatch_gate", ("task/preempt.rs", "linux_syscall_return")),
]
ENTER = re.compile(r"sync::entry::(?:enter(?:_user)?|fault_gate)\s*\(")
LEAVE = re.compile(r"sync::entry::(?:leave(?:_user)?|fault_gate)\s*\(")

# Gates that take no lock: (file, stub) -> why that is sound.
OPT_OUTS = {
    ("arch/idt.rs", "lapic_spurious_gate"): "does nothing (no EOI by definition)",
    ("arch/smp/ipi.rs", "resched_gate"): "IPI: sets a per-CPU flag",
    ("arch/smp/ipi.rs", "stop_gate"): "IPI: halts this CPU for a panic",
    ("arch/smp/shootdown.rs", "shootdown_gate"): "IPI: serves the per-CPU mailbox",
    ("arch/idt/faults.rs", "double_fault_gate"): "#DF: reports and halts, lock in any state",
}
OPT_OUT = re.compile(
    r"(?:irq_gate|paranoid_gate)!\s*\(\s*(\w+)\s*=>[^;]*?,\s*(?:window_safe|unlocked)\s*\)"
)
CALL = re.compile(r"\bcall\s+([A-Za-z_]\w*)")

# A halt with interrupts on, and where it may be: `sync::entry::halt` and its
# single-CPU stub.
HALT = re.compile(r"\benable_and_hlt\s*\(")
HALTS = {
    "sync/entry/imp.rs": 1,  # `halt`: marks, releases, halts, re-takes
    "sync/entry.rs": 1,  # the single-CPU stub of `halt`: nothing to release
}


def strip_comments(text: str) -> str:
    """Drop `/* .. */` and `// ..` comments (prose about an exit, the asm's
    own remarks). Neither appears inside the kernel's asm string literals."""
    text = re.sub(r"/\*.*?\*/", " ", text, flags=re.S)
    return re.sub(r"//[^\n]*", "", text)


def source(root: Path, rel: str) -> str | None:
    path = root / rel
    if not path.exists():
        return None
    return strip_comments(path.read_text(encoding="utf-8", errors="replace").replace("\r\n", "\n"))


def kernel_files(root: Path):
    for path in sorted(root.rglob("*.rs")):
        rel = path.relative_to(root).as_posix()
        if not rel.startswith("tests/"):
            yield rel


def scan(root: Path = KERNEL) -> dict[str, int]:
    """Exit counts per file, the suite excluded (its probes stage their own)."""
    found: dict[str, int] = {}
    for rel in kernel_files(root):
        count = len(EXIT.findall(source(root, rel) or ""))
        if count:
            found[rel] = count
    return found


def opt_outs(root: Path = KERNEL) -> set[tuple[str, str]]:
    found = set()
    for rel in kernel_files(root):
        if rel == "arch/gsgate.rs":
            continue  # the macros' own definitions
        for match in OPT_OUT.finditer(source(root, rel) or ""):
            found.add((rel, match.group(1)))
    return found


def body(root: Path, rel: str, name: str) -> str | None:
    """Top-level function `name` in `rel`, comments stripped: from its `fn`
    to the first `}` in column 0."""
    text = source(root, rel)
    if text is None:
        return None
    match = re.search(rf"\bfn {re.escape(name)}\s*[(<]", text)
    if not match:
        return None
    end = text.find("\n}", match.end())
    return text[match.start() : end if end != -1 else len(text)]


def exit_problems(found: dict[str, int]) -> list[str]:
    out = []
    for rel, count in found.items():
        allowed = ALLOWED.get(rel, 0)
        if count > allowed:
            out.append(
                f"{rel}: {count} iretq/sysretq, {allowed} allowed: its callee must take the "
                "entry lock (sync::entry) and be listed in check_entry.py"
            )
    for rel, allowed in ALLOWED.items():
        if found.get(rel, 0) < allowed:
            out.append(f"{rel}: allows {allowed} but holds {found.get(rel, 0)}: lower ALLOWED")
    return out


def call_problems(root: Path) -> list[str]:
    out = []
    for rel, wanted in STUB_CALLS.items():
        text = source(root, rel)
        if text is None:
            out.append(f"{rel}: missing (update STUB_CALLS)")
            continue
        calls = set(CALL.findall(text))
        for name in sorted(calls - wanted):
            out.append(f"{rel}: a stub calls {name}, which is not a hooked callee")
        for name in sorted(wanted - calls):
            out.append(f"{rel}: no stub calls {name} any more (update STUB_CALLS)")
    for rel, name, exit_at in HOOKED:
        text = body(root, rel, name)
        if text is None:
            out.append(f"{rel}: no fn {name} (moved? update HOOKED)")
            continue
        if not ENTER.search(text):
            out.append(f"{rel}: fn {name} does not call sync::entry::enter")
        exit_rel, exit_name = exit_at or (rel, name)
        exit_text = body(root, exit_rel, exit_name)
        if exit_text is None or not LEAVE.search(exit_text):
            out.append(f"{exit_rel}: fn {exit_name} does not call sync::entry::leave")
    return out


def opt_out_problems(found: set[tuple[str, str]]) -> list[str]:
    out = [
        f"{rel}: {stub} takes no entry lock: list it in OPT_OUTS with the reason, or lock it"
        for rel, stub in sorted(found - set(OPT_OUTS))
    ]
    out += [
        f"{rel}: {stub} is in OPT_OUTS but no longer opts out: remove it"
        for rel, stub in sorted(set(OPT_OUTS) - found)
    ]
    return out


def halts(root: Path = KERNEL) -> dict[str, int]:
    """`enable_and_hlt` calls per file, the suite excluded."""
    found: dict[str, int] = {}
    for rel in kernel_files(root):
        count = len(HALT.findall(source(root, rel) or ""))
        if count:
            found[rel] = count
    return found


def halt_problems(found: dict[str, int]) -> list[str]:
    out = [
        f"{rel}: {count} enable_and_hlt, {HALTS.get(rel, 0)} allowed: halt through "
        "sync::entry::halt, which drops the entry lock around the hlt"
        for rel, count in sorted(found.items())
        if count > HALTS.get(rel, 0)
    ]
    out += [
        f"{rel}: allows {allowed} enable_and_hlt but holds {found.get(rel, 0)}: lower HALTS"
        for rel, allowed in HALTS.items()
        if found.get(rel, 0) < allowed
    ]
    return out


def problems(found: dict[str, int], root: Path = KERNEL) -> list[str]:
    return (
        exit_problems(found)
        + call_problems(root)
        + opt_out_problems(opt_outs(root))
        + halt_problems(halts(root))
    )


def main() -> int:
    bad = problems(scan())
    for line in bad:
        print(f"check_entry: {line}", file=sys.stderr)
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
