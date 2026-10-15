"""Is a serial log complete? (issue #857)

The kernel never makes a writer wait for the serial ring: when it is full it
drops whole lines and says so with ``SERIAL:DROPPED``. A harness judges what it
finds in the log, so a log that carries that line cannot prove a marker was
*not* printed: every judge and session reads it through :func:`problems` and
fails the run, instead of reporting a missing marker as a product bug.
"""

from __future__ import annotations

import re

DROPPED = re.compile(r"SERIAL:DROPPED bytes=\d+ total=\d+")


def problems(text: str) -> list[str]:
    """One problem per ``SERIAL:DROPPED`` line (none for a complete log)."""
    return [f"serial output was dropped, the log is incomplete: {m.group(0)}"
            for m in DROPPED.finditer(text)]
