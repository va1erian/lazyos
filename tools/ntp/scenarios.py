"""The NTP harness's scenarios (docs/ntp-plan.md section 5): what the host
server does, the RTC the guest boots with, what is typed into the console,
and what the verdict checks.

Every scenario boots the same services console image
(`LAZYOS_SERVICES=1 LAZYOS_NETD=1`, `timed` started with
`ntp.server=10.0.2.2:PORT`), so one build serves all. Steps are
`qemu_session.py` steps; the console login is typed in as `user`/`lazy`.
A session user cannot write `sys/**`, so the configuration keys are not
driven from here (they need `elevd`'s prompt; `libs/sntp` and the Settings
page tests cover the key handling).
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Callable

import ntp_judge as judge
from fake_ntpd import Behaviour, honest

LOGIN = "LazyOS login: "
#: `libs/sntp/src/policy.rs` CLOCK_FLOOR_UNIX.
FLOOR = 1_767_225_600
STEP_OR_SYNC = r"TIMED:NTP:(STEP|SYNC)"
#: A little more than the 15 s minimum between two queries.
SPACING = 20


@dataclass
class Scenario:
    name: str
    what: str
    script: list[Behaviour]
    steps: list[dict]
    check: Callable[[judge.Context], list[str]]
    rtc_base: str | None = None


def boot() -> list[dict]:
    """Log in on the console as the development user."""
    return [
        {"wait_for": LOGIN, "timeout": 400},
        {"at": 1.0, "type": "user"},
        {"at": 1.6, "key": "enter"},
        {"at": 2.2, "type": "lazy"},
        {"at": 2.8, "key": "enter", "until": "LOGIN:OK:PASS user=user", "timeout": 60, "retries": 1},
        {"wait": 3.0},
    ]


def type_line(command: str, until: str | None = None, regex: bool = False,
              timeout: float = 60, retries: int = 3) -> list[dict]:
    enter: dict = {"key": "enter"}
    if until:
        enter.update(until=until, timeout=timeout, retries=retries, regex=regex)
    return [{"type": command, "delay": 0.05}, enter]


def now() -> list[dict]:
    return type_line("timectl now", r"unix_ms=\d+", regex=True)


def wait_event(pattern: str, timeout: float = 300, occurrence: int = 1) -> dict:
    return {"wait_for": pattern, "regex": True, "timeout": timeout, "occurrence": occurrence}


def sync() -> list[dict]:
    return [{"wait": SPACING}, *type_line("timectl ntp sync")]


# --- checks ------------------------------------------------------------------------


def check_dead_rtc(ctx: judge.Context) -> list[str]:
    problems = judge.request_shape(ctx.requests)
    floors, steps = ctx.of("FLOOR"), ctx.of("STEP")
    if len(floors) != 1:
        return problems + [f"expected one FLOOR line, saw {len(floors)}"]
    if floors[0].number("to") != FLOOR or (floors[0].number("from") or FLOOR) >= FLOOR:
        problems.append(f"FLOOR did not lift an old clock to {FLOOR}: {floors[0].text}")
    if len(steps) != 1:
        return problems + [f"expected one STEP, saw {len(steps)}"]
    step = steps[0]
    if ctx.events.index(floors[0]) > ctx.events.index(step):
        problems.append("the floor came after the network step")
    if "10.0.2.2" not in step.text:
        problems.append(f"the step names no 10.0.2.2 server: {step.text}")
    unix = step.number("unix")
    if unix is None or not ctx.window[0] - 20 <= unix <= ctx.window[1] + 20:
        problems.append(f"the step landed on {unix}, outside the host's {ctx.window}")
    if (step.number("stratum"), (step.number("delay_ms") or 0) < 1000) != (2, True):
        problems.append(f"stratum or delay wrong: {step.text}")
    return problems + judge.guest_clock_in_window(ctx)


def check_skew(ctx: judge.Context) -> list[str]:
    problems = judge.request_shape(ctx.requests)
    steps = ctx.of("STEP")
    if len(steps) != 1:
        return problems + [f"expected one STEP, saw {len(steps)}"]
    # The server is 3 s ahead; the guest's own RTC may be up to a second
    # behind the host's real time, so the offset reads 3.0 to 4.2 s.
    offset = steps[0].number("offset_ms") or 0
    if not 2700 <= offset <= 4500:
        problems.append(f"offset_ms={offset}, expected about 3000..4000")
    if ctx.of("REFUSE") or ctx.of("FAIL"):
        problems.append("a refusal or failure on an honest server")
    return problems


def check_resync(ctx: judge.Context) -> list[str]:
    problems = judge.request_shape(ctx.requests) + judge.minimum_interval(ctx.requests)
    acted = [e for e in ctx.events if e.kind in ("STEP", "SYNC")]
    if len(ctx.requests) < 2 or len(acted) < 2:
        return problems + [f"{len(ctx.requests)} requests, {len(acted)} results: no second exchange"]
    if ctx.of("REFUSE") or ctx.of("FAIL"):
        problems.append("a refusal or failure on an honest server")
    # Whatever the second one did, it was bounded like any later sync.
    offset = acted[1].number("offset_ms")
    if offset is None or abs(offset) > 60_000:
        problems.append(f"the second exchange measured {offset} ms")
    return problems + judge.guest_clock_in_window(ctx)


def check_refuse_large(ctx: judge.Context) -> list[str]:
    problems = judge.request_shape(ctx.requests) + judge.minimum_interval(ctx.requests)
    refusals = ctx.of("REFUSE")
    if not refusals or "too large" not in refusals[-1].text:
        return problems + [f"no 'too large' refusal: {[e.text for e in ctx.events]}"]
    if len(ctx.of("STEP")) > 1:
        problems.append("the +2 h answer stepped the clock")
    return problems + judge.guest_clock_in_window(ctx)


def check_rate(ctx: judge.Context) -> list[str]:
    problems = judge.request_shape(ctx.requests)
    if len(ctx.of("RATE")) != 1:
        problems.append(f"expected one RATE line, saw {len(ctx.of('RATE'))}")
    if len(ctx.requests) != 1:
        problems.append(f"{len(ctx.requests)} requests after a RATE kiss; the client must back off")
    if ctx.of("STEP") or ctx.of("SYNC"):
        problems.append("a clock action followed a kiss-o'-death")
    return problems


def check_deny(ctx: judge.Context) -> list[str]:
    """DENY: the address is remembered and never asked again this boot, even
    when the schedule comes round (a second attempt fails without a packet)."""
    problems = judge.request_shape(ctx.requests)
    if len(ctx.of("DENY")) != 1:
        problems.append(f"expected one DENY line, saw {len(ctx.of('DENY'))}")
    if len(ctx.requests) != 1:
        problems.append(f"{len(ctx.requests)} requests: a denied address was asked again")
    if not any("asked not to be queried" in e.text for e in ctx.of("FAIL")):
        problems.append("the retry did not stop at the denial")
    if ctx.of("STEP") or ctx.of("SYNC"):
        problems.append("a clock action followed a kiss-o'-death")
    return problems


def check_silent(ctx: judge.Context) -> list[str]:
    problems = judge.request_shape(ctx.requests) + judge.minimum_interval(ctx.requests)
    fails = ctx.of("FAIL")
    if len(fails) < 3 or len(ctx.requests) < 3:
        problems.append(f"{len(fails)} failures, {len(ctx.requests)} requests: it did not keep trying")
    g = judge.gaps(ctx.requests)
    # 5 s, 10 s, 20 s waits under the 15 s floor: about 15, 15, 20.
    if len(g) >= 3 and not g[2] > g[0] + 2.0:
        problems.append(f"the retry delay did not grow: {[round(x) for x in g]}")
    if ctx.of("STEP") or ctx.of("SYNC"):
        problems.append("a clock action without an answer")
    return problems


HOSTILE = ["nonce", "alarm", "stratum16", "zero", "garbage", "wrongport", "ancient"]


def check_hostile(ctx: judge.Context) -> list[str]:
    problems = judge.request_shape(ctx.requests)
    if ctx.of("STEP") or ctx.of("SYNC"):
        problems.append("a hostile server moved the clock or was believed")
    kinds = [e.kind for e in ctx.events if e.kind in ("FAIL", "REFUSE")]
    if len(kinds) < len(HOSTILE):
        problems.append(f"{len(kinds)} verdicts for {len(HOSTILE)} hostile answers")
    elif kinds[len(HOSTILE) - 1] != "REFUSE" or "floor" not in ctx.of("REFUSE")[-1].text:
        problems.append("the 1970 answer was not refused as before the floor")
    return problems + judge.guest_clock_in_window(ctx)


# --- the catalogue -----------------------------------------------------------------


def catalogue() -> list[Scenario]:
    hostile_steps: list[dict] = []
    for n in range(len(HOSTILE)):
        hostile_steps += [wait_event(r"TIMED:NTP:(FAIL|REFUSE)", 300, n + 1), *sync()]
    return [
        Scenario("dead_rtc", "a 2020 RTC: floored at start, stepped to the server's time",
                 [honest(0.0)], [*boot(), wait_event(r"TIMED:NTP:STEP"), *now()], check_dead_rtc,
                 rtc_base="2020-01-01T00:00:00"),
        Scenario("skew", "a server 3 s ahead: the first sync steps the clock",
                 [honest(3.0)], [*boot(), wait_event(r"TIMED:NTP:STEP"), *now()], check_skew),
        # Two honest answers 20 s apart. The guest's PIT-driven clock loses
        # ticks whenever the kernel holds interrupts off (QEMU, any
        # accelerator), so the second exchange may find it 100 ms or 5 s
        # behind: it must be taken (stepped, or left alone under 250 ms) and
        # the clock must end up where the host's is.
        Scenario("resync", "a second exchange 20 s later is taken and the clock tracks the host",
                 [honest(0.0), honest(0.0)],
                 [*boot(), wait_event(STEP_OR_SYNC), *sync(),
                  wait_event(STEP_OR_SYNC, 120, 2), *now()],
                 check_resync),
        Scenario("refuse_large", "a later answer 2 h away is refused, the clock stays",
                 [honest(0.0), honest(7200.0)],
                 [*boot(), wait_event(STEP_OR_SYNC), *sync(), wait_event(r"TIMED:NTP:REFUSE", 120),
                  *now()], check_refuse_large),
        Scenario("rate", "a RATE kiss: one request, then silence",
                 [Behaviour("rate")],
                 [*boot(), wait_event(r"TIMED:NTP:RATE"), {"wait": 40}], check_rate),
        Scenario("deny", "a DENY kiss: that address is not asked again",
                 [Behaviour("deny")],
                 [*boot(), wait_event(r"TIMED:NTP:DENY"),
                  wait_event(r"TIMED:NTP:FAIL every server address", 120), *now()],
                 check_deny),
        Scenario("silent", "no answers: retries back off, never closer than 15 s",
                 [Behaviour("silent")],
                 [*boot(), wait_event(r"TIMED:NTP:FAIL", 300, 3), {"wait": 1}], check_silent),
        Scenario("hostile", "forged, alarmed, zeroed, foreign and 1970 answers move nothing",
                 [Behaviour(kind) for kind in HOSTILE],
                 [*boot(), *hostile_steps[:-len(sync())], wait_event(r"TIMED:NTP:REFUSE", 300),
                  *now()], check_hostile),
    ]
