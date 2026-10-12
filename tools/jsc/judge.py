"""Judge the serial log of a ``jsc`` fixture boot (``tools/abi/fixtures/src/jsc.rs``).

Pure text in, verdicts out, so ``test_judge.py`` can show it fails when it
should. A marker the guest never printed is a failure, not a pass: a boot
that hangs or crashes must not look like success.

Markers: ``JSC:W1:PASS:cxx``, ``JSC:W1:PASS:icu``, ``JSC:W1:PASS`` (the C++
toolchain and ICU, stage W1); ``JSC:UP:PASS``, ``JSC:START:<ms>`` and
``JSC:RUN:PASS:<name>`` for ``arith``, ``gc`` and ``promise`` (the shell,
stage W2), each script followed by ``JSC:TIME:<name>:<ms>``; ``JSC:W1:FAIL:...`` / ``JSC:RUN:FAIL:<name>:<reason>`` say why not.
"""

from __future__ import annotations

import re
from dataclasses import dataclass

W1_MARKERS = ("JSC:W1:PASS:cxx", "JSC:W1:PASS:icu", "JSC:W1:PASS")
RUNS = ("arith", "gc", "promise")


@dataclass(frozen=True)
class Verdict:
    name: str
    ok: bool
    detail: str = ""


def has_line(serial: str, marker: str) -> bool:
    """Whether a whole line is `marker` (so `...PASS` is not `...PASS:cxx`)."""
    return re.search(rf"^{re.escape(marker)}\s*$", serial, re.M) is not None


def failures(serial: str) -> list[str]:
    """Every failure line the guest printed, trimmed."""
    found = re.findall(r"^(JSC:(?:W1|RUN|UP):FAIL.*)$", serial, re.M)
    killed = re.findall(r"^(user: task \d+ killed by.*)$", serial, re.M)
    return [line.strip()[:200] for line in found + killed]


def judge(serial: str, stages: tuple[str, ...] = ("w1", "w2")) -> list[Verdict]:
    verdicts: list[Verdict] = []
    bad = failures(serial)
    if "w1" in stages:
        for marker in W1_MARKERS:
            verdicts.append(Verdict(marker, has_line(serial, marker)))
    if "w2" in stages:
        verdicts.append(Verdict("JSC:UP:PASS", has_line(serial, "JSC:UP:PASS")))
        for name in RUNS:
            verdicts.append(Verdict(f"JSC:RUN:PASS:{name}",
                                    has_line(serial, f"JSC:RUN:PASS:{name}")))
        # The fixture only passes as a whole when the shell did too.
        verdicts.append(Verdict("ABI:jsc:PASS", has_line(serial, "ABI:jsc:PASS")))
    # Stage w1 alone ignores the shell's failures (no jsc built yet).
    if "w2" in stages:
        shown = bad
    else:
        shown = [line for line in bad if line.startswith("JSC:W1:") or line.startswith("user:")]
    verdicts.append(Verdict("no failure lines", not shown, "; ".join(shown)))
    return verdicts


def startup_ms(serial: str) -> int | None:
    """How long the guest's first ``jsc -e`` took, from ``JSC:START:<ms>``."""
    found = re.search(r"^JSC:START:(\d+)\s*$", serial, re.M)
    return int(found.group(1)) if found else None


def times(serial: str) -> dict[str, int]:
    """Milliseconds each script took in the guest, from ``JSC:TIME:<name>:<ms>``."""
    return {name: int(ms) for name, ms in re.findall(r"^JSC:TIME:(\w+):(\d+)\s*$", serial, re.M)}


def passed(verdicts: list[Verdict]) -> bool:
    return all(v.ok for v in verdicts)
