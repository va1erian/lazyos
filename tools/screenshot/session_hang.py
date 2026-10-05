"""Hang evidence for qemu_session.py: what a stuck guest was doing (issue #382)."""

from __future__ import annotations

import sys
import time
from pathlib import Path


def capture_hang_state(qmp, out_dir: Path, serial, settle: float = 2.0) -> None:
    """Record why a guest stopped making progress.

    A hung kernel is usually spinning with interrupts off, so its serial log
    just stops. The monitor's ``info registers`` shows where the CPU is from
    outside (saved to ``hang_registers.txt``), and an injected NMI makes the
    kernel print its ``HANG:`` report (``kernel/src/arch/nmi.rs``) into the
    serial log; its lines are echoed here too.
    """
    try:
        # The stack words matter where an accelerator drops injected NMIs
        # (WHPX does): they are then the only view of the call chain.
        state = "\n".join(
            qmp.execute("human-monitor-command", **{"command-line": command})
            for command in ("info registers", "x /48gx $rsp")
        )
        (out_dir / "hang_registers.txt").write_text(state, encoding="utf-8")
        print("--- info registers ---", file=sys.stderr)
        print(state, file=sys.stderr, flush=True)
        qmp.execute("inject-nmi")
        time.sleep(settle)
        report = [line for line in serial.text().splitlines() if "HANG:" in line]
        print("--- hang report ---", file=sys.stderr)
        print("\n".join(report) or "(no HANG: lines)", file=sys.stderr, flush=True)
    except Exception as error:
        print(f"(could not capture hang state: {error})", file=sys.stderr)
