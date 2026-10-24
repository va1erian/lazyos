"""Following the kernel suite's serial log, and reporting a stall (issue #787).

Split from `run.py`: the stall report names the test in flight (the kernel
prints `TEST:<name>:START` before each test) and the guest's RIP, so a run
that hangs says where instead of only timing out.
"""

from __future__ import annotations

import re
import subprocess
import time
from pathlib import Path
from typing import Any

RE_RESULT = re.compile(r"^TEST:([^:]+):(PASS|FAIL)(?::(.*))?$")
RE_START = re.compile(r"^TEST:([^:]+):START$")
RE_RIP = re.compile(r"RIP=([0-9a-f]{16})")


def running_test(text: str) -> str | None:
    """The test the kernel started and has not finished (its `START` line is
    the last line naming a test, with no `PASS`/`FAIL` after it)."""
    running = None
    for line in text.splitlines():
        line = line.strip()
        started = RE_START.match(line)
        if started:
            running = started.group(1)
            continue
        finished = RE_RESULT.match(line)
        if finished and finished.group(1) == running:
            running = None
    return running


def sample_rips(qmp: Any, samples: int = 3) -> list[str]:
    """The guest's RIP a few times a second apart (a spin shows one place or a
    few, a halt the idle loop), for a stall report. Empty without QMP."""
    rips: list[str] = []
    for index in range(samples if qmp is not None else 0):
        if index:
            time.sleep(1.0)
        try:
            regs = qmp.execute("human-monitor-command", **{"command-line": "info registers -a"})
        except Exception as exc:  # QEMU busy or gone: say so, keep waiting
            rips.append(f"unavailable ({exc})")
            break
        found = RE_RIP.findall(regs if isinstance(regs, str) else "")
        rips.append("/".join(found) if found else "unreadable")  # one per vCPU
    return rips


def stall_report(text: str, silent_for: float, qmp: Any, nmi: bool = False) -> str:
    """Evidence for a run whose serial log stopped growing: the test in
    flight, the log's tail and where the guest is executing. Symbolize a RIP
    with `addr2line -e <kernel> <rip - 0xffff800000000000>`."""
    tail = [line for line in text.splitlines() if line.strip()][-8:]
    lines = [
        f"STALL: no serial output for {silent_for:.0f} s; test in flight: "
        f"{running_test(text) or 'none (between tests, or before the first)'}",
        "STALL: last serial lines:",
        *[f"STALL:   {line}" for line in tail],
        "STALL: guest RIP samples: " + (", ".join(sample_rips(qmp)) or "no QMP"),
    ]
    if nmi and qmp is not None:
        # The kernel's NMI handler prints its `HANG:` view (the scheduler's
        # last tick, every task, the locks) to the serial log.
        try:
            qmp.execute("inject-nmi")
            lines.append("STALL: NMI sent: the HANG: lines follow in the serial log")
        except Exception as exc:
            lines.append(f"STALL: NMI not sent ({exc})")
    return "\n".join(lines)


def emit_stall(text: str, silent: float, qmp: Any, stall_file: Path | None) -> None:
    """Print and save a stall report. Evidence must never end the run it is
    about (a console that cannot encode the guest's bytes, a full disk), so
    any failure here is reported and the wait goes on."""
    try:
        report = stall_report(text, silent, qmp)
        print(report.encode("ascii", "backslashreplace").decode("ascii"), flush=True)
        if stall_file is not None:
            with stall_file.open("a", encoding="utf-8") as handle:
                handle.write(report + "\n")
    except Exception as exc:
        print(f"STALL: report failed: {exc!r}", flush=True)


def follow_serial(
    serial_log: Path,
    proc: subprocess.Popen,
    deadline: float,
    qmp: Any = None,
    stall_after: float = 0.0,
    stall_file: Path | None = None,
    stall_nmi: bool = False,
) -> str:
    """Poll the serial log (streaming TEST lines) until the summary or deadline.

    A log that stops growing for `stall_after` seconds (0: never) gets one
    `stall_report` per stall, printed and appended to `stall_file` (with
    `stall_nmi`, after an NMI that makes the kernel print its `HANG:` view;
    off by default, because suites that count NMIs would see it). It is
    evidence only: the wait goes on until the deadline, because a slow test on
    a loaded host looks the same for a while."""
    consumed = 0
    text = ""
    last_size, last_growth, reported = -1, time.time(), False
    while time.time() < deadline:
        if serial_log.is_file():
            text = serial_log.read_text(errors="replace")
            if len(text) != last_size:
                last_size, last_growth, reported = len(text), time.time(), False
            complete = text.count("\n")
            for line in text.splitlines()[consumed:complete]:
                if RE_START.match(line):
                    continue
                if line.startswith("TEST:") or "PANIC" in line or "EXCEPTION" in line:
                    print(f"  {line}", flush=True)
            consumed = complete
            if "TEST:SUMMARY:" in text:
                return text
        silent = time.time() - last_growth
        if stall_after and silent >= stall_after and not reported and proc.poll() is None:
            reported = True
            report = stall_report(text, silent, qmp, nmi=stall_nmi)
            print(report, flush=True)
            if stall_file is not None:
                with stall_file.open("a", encoding="utf-8") as handle:
                    handle.write(report + "\n")
        if proc.poll() is not None:
            time.sleep(0.5)  # let the last writes reach the file
            if serial_log.is_file():
                text = serial_log.read_text(errors="replace")
            return text
        time.sleep(0.25)
    return text
