"""Judge the serial log of a ``webkit-smoke`` boot (W4, ``webkit/port/smoke/main.cpp``).

Pure text in, verdicts out, so ``test_port_judge.py`` can show it fails when it
should. A marker the guest never printed is a failure, not a pass; so is a
``WEBKIT:W4:PASS`` without the lines that justify it.

Markers, in the order the program prints them::

    WEBKIT:W4:UP
    WEBKIT:W4:WEBPROCESS:UP:<pid>        WEBKIT:W4:NETWORKPROCESS:UP:<pid>
    WEBKIT:W4:LOAD:about:blank           WEBKIT:W4:LOAD:<data url>
    WEBKIT:W4:TITLE:hello lazyos
    WEBKIT:W4:PAINT:<w>x<h>:<crc>        (only when pixels arrived)
    WEBKIT:W4:INFO:PAINT:pixels=<n>:displayCallbacks=<n>
    WEBKIT:W4:INFO:INK:pixels=<n>:bbox=<x0>,<y0>,<x1>,<y1>     (the glyphs, as ART lines too)
    WEBKIT:W4:PASS     or     WEBKIT:W4:FAIL:<stage>:<reason>

The stage reached is reported even on failure: the highest marker present.

``tools/webkit/port.py`` also runs ``webkit-smoke`` once on the Linux build host (the programs are
static musl executables; ``target/webkit/smoke.host.txt``). Given that run, the judge requires the
guest to have painted the same bitmap: the checksum and the ink must be equal, which shows WebKit's
layout, Skia and the fonts compute the same page on LazyOS as on Linux.
"""

from __future__ import annotations

import re
from dataclasses import dataclass

TITLE = "hello lazyos"
DATA_URL_PREFIX = "data:text/html,"
#: The stages in order, for `reached`.
STAGES = ("UP", "WEBPROCESS", "NETWORKPROCESS", "LOAD", "TITLE", "PAINT", "PASS")


@dataclass(frozen=True)
class Verdict:
    name: str
    ok: bool
    detail: str = ""


def lines(serial: str, prefix: str) -> list[str]:
    """The text after `prefix` of every line of the log that starts with it."""
    return [m.group(1).rstrip() for m in re.finditer(rf"^{re.escape(prefix)}(.*)$", serial, re.M)]


def has_line(serial: str, marker: str) -> bool:
    return re.search(rf"^{re.escape(marker)}\s*$", serial, re.M) is not None


def failures(serial: str) -> list[str]:
    """Every failure the guest reported or suffered, trimmed."""
    found = re.findall(r"^(WEBKIT:W4:FAIL.*)$", serial, re.M)
    killed = re.findall(r"^(user: task \d+ killed by.*)$", serial, re.M)
    crashed = re.findall(r"^(user: code at .*)$", serial, re.M)
    return [line.strip()[:200] for line in found + killed + crashed]


def reached(serial: str) -> str:
    """The furthest stage the log shows (``none`` if the program never started)."""
    best = "none"
    for stage in STAGES:
        pattern = rf"^WEBKIT:W4:{stage}\b" if stage != "UP" else r"^WEBKIT:W4:UP\s*$"
        if re.search(pattern, serial, re.M):
            best = stage
    return best


def paint(serial: str) -> tuple[str, str] | None:
    """``(size, crc)`` of the PAINT line, or None."""
    found = re.findall(r"^WEBKIT:W4:PAINT:(\d+x\d+):([0-9a-f]{8})\s*$", serial, re.M)
    return found[-1] if found else None


def painted_pixels(serial: str) -> int:
    found = re.findall(r"^WEBKIT:W4:INFO:PAINT:pixels=(\d+)", serial, re.M)
    return int(found[-1]) if found else 0


def ink(serial: str) -> str | None:
    """The ``pixels=<n>:bbox=...`` of the INK line, or None."""
    found = re.findall(r"^WEBKIT:W4:INFO:INK:(\S+)\s*$", serial, re.M)
    return found[-1] if found else None


def judge(serial: str, host: str | None = None) -> list[Verdict]:
    """Verdicts for one boot; `host` is the build host's own run of the same program, if any."""
    verdicts = [Verdict("WEBKIT:W4:UP", has_line(serial, "WEBKIT:W4:UP"))]
    for name in ("WEBPROCESS", "NETWORKPROCESS"):
        pids = [p for p in lines(serial, f"WEBKIT:W4:{name}:UP:") if p.isdigit() and int(p) > 0]
        verdicts.append(Verdict(f"WEBKIT:W4:{name}:UP", bool(pids), f"pid {pids[-1]}" if pids else ""))
    loads = lines(serial, "WEBKIT:W4:LOAD:")
    verdicts.append(Verdict("WEBKIT:W4:LOAD:about:blank", "about:blank" in loads))
    data = [url for url in loads if url.startswith(DATA_URL_PREFIX)]
    verdicts.append(Verdict("WEBKIT:W4:LOAD:<data url>", bool(data), data[-1] if data else ""))
    titles = lines(serial, "WEBKIT:W4:TITLE:")
    verdicts.append(Verdict(f"WEBKIT:W4:TITLE:{TITLE}", TITLE in titles, f"got {titles}" if titles != [TITLE] else ""))
    drawn = paint(serial)
    pixels = painted_pixels(serial)
    if drawn or pixels:
        verdicts.append(Verdict("WEBKIT:W4:PAINT with the pixels it counted", bool(drawn) and pixels > 0,
                                f"{drawn} pixels={pixels}"))
    if drawn:
        verdicts.append(Verdict("the page has ink (text was drawn)", (ink(serial) or "pixels=0").split(":")[0] != "pixels=0",
                                ink(serial) or ""))
    if host:
        want = paint(host)
        verdicts.append(Verdict("paint equals the host's", want is not None and drawn == want,
                                f"guest {drawn}, host {want}"))
        verdicts.append(Verdict("ink equals the host's", ink(host) is not None and ink(serial) == ink(host),
                                f"guest {ink(serial)}, host {ink(host)}"))
    verdicts.append(Verdict("WEBKIT:W4:PASS", has_line(serial, "WEBKIT:W4:PASS")))
    bad = failures(serial)
    verdicts.append(Verdict("no failure lines", not bad, "; ".join(bad)))
    return verdicts


def passed(verdicts: list[Verdict]) -> bool:
    return all(v.ok for v in verdicts)
