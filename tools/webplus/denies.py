"""The `LABEL:DENY` lines a LazyWeb+ run may print (issue #835, #836's note on the WebKit browser).

The page process and the network process run confined (docs/architecture/process-sandbox.md); the
engine looks up a few optional things and falls back when the kernel says no. Those denials are
documented and left alone: the tables are not widened to silence them (the page process must not
write the fonts directory). Any other denial of either child, or of the app, is a missing permission
or an attack, and fails the run (`python tools/webplus/run.py --label-trace`).
"""

from __future__ import annotations

import re

APP = "os.lazy.lazywebplus"

#: (label suffix, regex of the rest of the line): what is allowed to be refused.
OPTIONAL = [
    # ICU's time zone lookups: it falls back to UTC.
    ("webprocess", r"file=/etc access=stat"),
    ("webprocess", r"file=/etc/localtime access=stat"),
    ("webprocess", r"file=/usr/share/zoneinfo access=stat"),
    # OpenSSL's optional configuration.
    ("networkprocess", r"file=/etc/ssl/openssl\.cnf access=stat"),
    # Fontconfig: a configuration directory baked in at build time, absent here, and its cache, which it
    # then does not write (the page process may only read the fonts directory).
    ("webprocess", r"file=/\S*/fontconfig/conf\.avail access=read"),
    ("webprocess", r"file=/system/share/fonts(/\S+)? access=write"),
    ("webprocess", r"file=/tmp(/fontconfig)? access=write"),
]

LINE = re.compile(r"LABEL:DENY label=(?P<label>\S+) (?P<what>.*?)\s*$")


def unexpected(lines: list[str]) -> list[str]:
    """The distinct `LABEL:DENY` lines of `lines` that are not in the documented optional set."""
    found = []
    for line in sorted(set(lines)):
        match = LINE.search(line)
        if match and any(match["label"] == f"app:{APP}/{child}" and re.fullmatch(rest, match["what"])
                         for child, rest in OPTIONAL):
            continue
        found.append(line)
    return found
