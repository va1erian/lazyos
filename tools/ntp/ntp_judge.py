"""The NTP harness's verdicts (docs/ntp-plan.md section 5), pure functions
over the guest's serial log, the host server's request log and the
`timectl now` readings, so `test_judge.py` can prove they fail when they
should.

The guest prints (serial channel `TIMED`, `user/src/bin/timed/ntp.rs`):

    TIMED:NTP:FLOOR from=<unix> to=<unix>
    TIMED:NTP:STEP offset_ms=<n> delay_ms=<n> stratum=<n> server=<label> unix=<n>
    TIMED:NTP:SYNC offset_ms=<n> delay_ms=<n> stratum=<n> server=<label>
    TIMED:NTP:REFUSE <reason> offset_ms=<n> server=<label>
    TIMED:NTP:FAIL <why>
    TIMED:NTP:RATE asked=<n>
"""

from __future__ import annotations

import re
from dataclasses import dataclass, field

#: Not anchored to the start of the line: the guest's shell prompt (`~ $ `)
#: shares a line with whatever a service prints while it waits.
EVENT = re.compile(r"TIMED:NTP:(FLOOR:FAIL|FLOOR|STEP|SYNC|REFUSE|FAIL|RATE|DENY)\b ?(.*)$", re.M)
NUMBER = re.compile(r"\b(\w+)=(-?\d+)\b")
READING = re.compile(r"^unix_ms=(\d+) offset=", re.M)
#: The least time between two queries (`libs/sntp/src/schedule.rs`).
MIN_INTERVAL_S = 15.0


@dataclass
class Event:
    kind: str
    text: str
    numbers: dict[str, int]

    def number(self, name: str) -> int | None:
        return self.numbers.get(name)


def events(serial: str) -> list[Event]:
    """Every `TIMED:NTP:*` line, in order."""
    found = []
    for match in EVENT.finditer(serial):
        text = match.group(2).strip()
        found.append(Event(match.group(1), text, {k: int(v) for k, v in NUMBER.findall(text)}))
    return found


def readings(serial: str) -> list[int]:
    """The `unix_ms=` values `timectl now` printed."""
    return [int(m.group(1)) for m in READING.finditer(serial)]


@dataclass
class Context:
    serial: str
    requests: list  # fake_ntpd.Request
    window: tuple[float, float]   # host time.time() before the boot and after the session
    events: list[Event] = field(default_factory=list)
    readings: list[int] = field(default_factory=list)

    def __post_init__(self) -> None:
        self.events = events(self.serial)
        self.readings = readings(self.serial)

    def of(self, kind: str) -> list[Event]:
        return [e for e in self.events if e.kind == kind]


def timed_started_once(serial: str) -> list[str]:
    """`timed` came up exactly once. `init` restarts it when it dies, and a
    restart quietly makes the next exchange a first sync again, so a crash
    (a hostile packet, say) must not pass unseen."""
    starts = serial.count("TIMED:READY")
    return [] if starts == 1 else [f"timed announced itself {starts} times (a restart?)"]


def request_shape(requests: list) -> list[str]:
    """What a correct client sends: 48 bytes, version 4, mode 3, one random
    transmit timestamp per request, nothing else set."""
    problems = []
    for r in requests:
        if r.length != 48:
            problems.append(f"request {r.index}: {r.length} bytes, not 48")
        if (r.version, r.mode) != (4, 3):
            problems.append(f"request {r.index}: version {r.version} mode {r.mode}, not 4 and 3")
        if r.transmit == bytes(8):
            problems.append(f"request {r.index}: no nonce in the transmit timestamp")
        if r.other_nonzero:
            problems.append(f"request {r.index}: a header field other than the nonce is set")
    nonces = [r.transmit for r in requests]
    if len(set(nonces)) != len(nonces):
        problems.append("two requests carried the same nonce")
    return problems


def gaps(requests: list) -> list[float]:
    return [b.at - a.at for a, b in zip(requests, requests[1:])]


def minimum_interval(requests: list) -> list[str]:
    """No two queries closer than the rate limit (a little slack for the
    host's scheduling and the guest's clock)."""
    return [f"requests {i} and {i + 1} were {g:.1f} s apart (minimum {MIN_INTERVAL_S:.0f} s)"
            for i, g in enumerate(gaps(requests)) if g < MIN_INTERVAL_S - 1.0]


def guest_clock_in_window(ctx: Context, slack: float = 20.0) -> list[str]:
    """The last `timectl now` reading lies within the host's run window."""
    if not ctx.readings:
        return ["the guest never printed its time (`timectl now`)"]
    seen = ctx.readings[-1] / 1000.0
    lo, hi = ctx.window[0] - slack, ctx.window[1] + slack
    if not lo <= seen <= hi:
        return [f"the guest clock reads {seen:.0f}, outside the host's {ctx.window[0]:.0f}..{ctx.window[1]:.0f}"]
    return []
