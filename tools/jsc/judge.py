"""Judge the serial log of a ``jsc`` fixture boot (``tools/abi/fixtures/src/jsc.rs``).

Pure text in, verdicts out, so ``test_judge.py`` can show it fails when it
should. A marker the guest never printed is a failure, not a pass: a boot
that hangs or crashes must not look like success.

Markers: ``JSC:W1:PASS:cxx``, ``JSC:W1:PASS:icu``, ``JSC:W1:PASS`` (the C++
toolchain and ICU, stage W1). Stage W2 runs two shells, the C-loop ``jsc``
(prefix ``JSC:``) and the JIT ``jsc-jit`` (prefix ``JSC:JIT:``); each prints
``<prefix>UP:PASS``, ``<prefix>START:<ms>`` and ``<prefix>RUN:PASS:<name>`` for
``arith``, ``gc``, ``promise``, ``hot``, ``agents`` and ``suspend`` (each script followed by
``<prefix>TIME:<name>:<ms>``), and ``<prefix>UP:FAIL:...`` /
``<prefix>RUN:FAIL:<name>:<reason>`` say why not. Then the stress subset
(``tools/jsc/stress.py``): ``<prefix>STRESS:<pass>/<total>``, one
``<prefix>STRESS:FAIL:<name>:<reason>`` per failing file: a name outside
:data:`KNOWN_STRESS_FAILURES` fails the shell, and the pass rate must still
reach :data:`STRESS_MIN_RATE` of at least :data:`STRESS_MIN_TOTAL` files (the
backstop for a file that dies without printing). The kernel's per-thread
signal path is guarded by ``<prefix>SIGNALS:<name>:<count>``, the number of
thread-directed signals (``tkill``/``tgkill``) the shell's process had sent
by the end of the script, read from ``/proc/self/status``: the ``suspend``
script must show more than zero (:data:`SIGNAL_RUNS`).
"""

from __future__ import annotations

import re
from dataclasses import dataclass

W1_MARKERS = ("JSC:W1:PASS:cxx", "JSC:W1:PASS:icu", "JSC:W1:PASS")
RUNS = ("arith", "gc", "promise", "hot", "agents", "suspend")
#: The two shells: marker prefix and the program name.
SHELLS = {"JSC:": "jsc", "JSC:JIT:": "jsc-jit"}

#: The stress files the image must carry (the subset is 255), and the share of
#: them that must pass in each shell. Set from the first full run
#: (docs/webkit-port-plan.md section 5, W2); a drop below it is a regression.
STRESS_MIN_TOTAL = 200
STRESS_MIN_RATE = {"JSC:": 0.98, "JSC:JIT:": 0.98}
#: The one stress file known to fail, in both shells: `memoryHog` zero-fills a
#: 2 GiB typed array and the default 2 GiB guest runs out of frames
#: (`--memory 4G` passes it). Any other failing name is a regression, however
#: high the rate stays.
KNOWN_STRESS_FAILURES = frozenset({"get-array-length-reuse.js"})
#: The scripts that must make the process send thread-directed signals: the
#: collector suspends a mutator parked outside the interpreter with `tgkill`
#: (`suspend`: docs/webkit-port-plan.md 4.2). The others report their count
#: but may legitimately send none.
SIGNAL_RUNS = ("suspend",)


@dataclass(frozen=True)
class Verdict:
    name: str
    ok: bool
    detail: str = ""


def has_line(serial: str, marker: str) -> bool:
    """Whether a whole line is `marker` (so `...PASS` is not `...PASS:cxx`)."""
    return re.search(rf"^{re.escape(marker)}\s*$", serial, re.M) is not None


def failures(serial: str) -> list[str]:
    """Every failure line the fixture printed, trimmed.

    The kernel's ``user: task N killed by ...`` lines are not among them: a
    stress file that crashes prints one and is already counted by the stress
    rate, and a crash anywhere else makes the fixture print a failure line.
    """
    found = re.findall(r"^(JSC:(?:JIT:)?(?:W1|RUN|UP):FAIL.*)$", serial, re.M)
    return [line.strip()[:200] for line in found]


def stress(serial: str, prefix: str) -> tuple[int, int] | None:
    """``(passed, total)`` from ``<prefix>STRESS:<pass>/<total>``, if printed."""
    found = re.search(rf"^{re.escape(prefix)}STRESS:(\d+)/(\d+)\s*$", serial, re.M)
    return (int(found.group(1)), int(found.group(2))) if found else None


def stress_failures(serial: str, prefix: str) -> dict[str, str]:
    """``{name: reason}`` of the stress files that failed under `prefix`'s shell."""
    pattern = rf"^{re.escape(prefix)}STRESS:FAIL:([^:\s]+):(.*)$"
    return {name: reason.strip() for name, reason in re.findall(pattern, serial, re.M)}


def judge_stress(serial: str, prefix: str) -> Verdict:
    name = f"{prefix}STRESS"
    counted = stress(serial, prefix)
    if counted is None:
        return Verdict(name, False, "no summary line")
    passed, total = counted
    if total < STRESS_MIN_TOTAL:
        return Verdict(name, False, f"only {total} files ran, want {STRESS_MIN_TOTAL}")
    floor = STRESS_MIN_RATE[prefix]
    shown = f"{passed}/{total} ({passed / total:.1%}), want {floor:.1%}"
    unexpected = sorted(set(stress_failures(serial, prefix)) - KNOWN_STRESS_FAILURES)
    if unexpected:
        return Verdict(name, False, f"unexpected failures: {', '.join(unexpected)}; {shown}")
    return Verdict(name, passed / total >= floor, shown)


def signals(serial: str, prefix: str = "JSC:") -> dict[str, int]:
    """Thread-directed signals sent per script, from ``<prefix>SIGNALS:<name>:<count>``."""
    pattern = rf"^{re.escape(prefix)}SIGNALS:(\w+):(\d+)\s*$"
    return {name: int(count) for name, count in re.findall(pattern, serial, re.M)}


def judge_signals(serial: str, prefix: str, name: str) -> Verdict:
    count = signals(serial, prefix).get(name)
    if count is None:
        return Verdict(f"{prefix}SIGNALS:{name}", False, "no count line")
    return Verdict(f"{prefix}SIGNALS:{name}", count > 0, f"{count} sent")


def judge(serial: str, stages: tuple[str, ...] = ("w1", "w2")) -> list[Verdict]:
    verdicts: list[Verdict] = []
    bad = failures(serial)
    if "w1" in stages:
        for marker in W1_MARKERS:
            verdicts.append(Verdict(marker, has_line(serial, marker)))
    if "w2" in stages:
        for prefix in SHELLS:
            verdicts.append(Verdict(f"{prefix}UP:PASS", has_line(serial, f"{prefix}UP:PASS")))
            for name in RUNS:
                marker = f"{prefix}RUN:PASS:{name}"
                verdicts.append(Verdict(marker, has_line(serial, marker)))
            for name in SIGNAL_RUNS:
                verdicts.append(judge_signals(serial, prefix, name))
            verdicts.append(judge_stress(serial, prefix))
        # The fixture only passes as a whole when the shells did too.
        verdicts.append(Verdict("ABI:jsc:PASS", has_line(serial, "ABI:jsc:PASS")))
    # Stage w1 alone ignores the shell's failures (no jsc built yet).
    if "w2" in stages:
        shown = bad
    else:
        shown = [line for line in bad if line.startswith("JSC:W1:")]
    verdicts.append(Verdict("no failure lines", not shown, "; ".join(shown)))
    return verdicts


def startup_ms(serial: str, prefix: str = "JSC:") -> int | None:
    """How long the guest's first ``jsc -e`` took, from ``<prefix>START:<ms>``."""
    found = re.search(rf"^{re.escape(prefix)}START:(\d+)\s*$", serial, re.M)
    return int(found.group(1)) if found else None


def times(serial: str, prefix: str = "JSC:") -> dict[str, int]:
    """Milliseconds each script took in the guest, from ``<prefix>TIME:<name>:<ms>``."""
    pattern = rf"^{re.escape(prefix)}TIME:(\w+):(\d+)\s*$"
    return {name: int(ms) for name, ms in re.findall(pattern, serial, re.M)}


def stress_ms(serial: str, prefix: str = "JSC:") -> int | None:
    """Milliseconds the stress subset took, from ``<prefix>STRESS:TIME:<ms>``."""
    found = re.search(rf"^{re.escape(prefix)}STRESS:TIME:(\d+)\s*$", serial, re.M)
    return int(found.group(1)) if found else None


def passed(verdicts: list[Verdict]) -> bool:
    return all(v.ok for v in verdicts)
