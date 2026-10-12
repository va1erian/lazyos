"""Judge the serial log of a ``depsproof`` boot (``tools/webkit/depsproof``).

Pure text in, verdicts out, so ``test_judge.py`` can show it fails when it
should. A marker the guest never printed is a failure, not a pass.

Markers: ``WEBKIT:W3:PASS:<lib>`` or ``WEBKIT:W3:FAIL:<lib>:<reason>`` for each
library, ``WEBKIT:W3:INFO:<lib>:<key>=<value>`` evidence, and a last
``WEBKIT:W3:PASS`` once every library passed (``WEBKIT:W3:OFFLINE`` is the
host reference run, which has no network and therefore never counts).

The program is a static musl executable, so the build runs it on the build
host too (``target/webkit/depsproof.host.txt``); the guest must print the same
INFO values for everything deterministic (decoded pixels, the glyph bitmap,
the shaping result, the Skia surface checksum), which shows the libraries
compute the same thing on LazyOS as on Linux.
"""

from __future__ import annotations

import re
from dataclasses import dataclass

#: The libraries that must pass, in the order the program checks them.
LIBS = ("zlib", "brotli", "icu", "libpng", "libjpeg-turbo", "libwebp", "libxml2", "expat", "sqlite",
        "freetype", "harfbuzz", "fontconfig", "skia", "libpsl", "openssl", "nghttp2", "curl")
#: INFO values the guest and the host must agree on.
COMPARED = (("zlib", "crc32"), ("libpng", "pixels"), ("libjpeg-turbo", "pixels"), ("libwebp", "pixels"),
            ("freetype", "glyph-A"), ("harfbuzz", "glyphs"), ("harfbuzz", "advance"),
            ("skia", "pixels"), ("skia", "inked"))


@dataclass(frozen=True)
class Verdict:
    name: str
    ok: bool
    detail: str = ""


def has_line(serial: str, marker: str) -> bool:
    """Whether a whole line is `marker` (so `...PASS` is not `...PASS:zlib`)."""
    return re.search(rf"^{re.escape(marker)}\s*$", serial, re.M) is not None


def info(serial: str) -> dict[tuple[str, str], str]:
    """Every ``WEBKIT:W3:INFO:<lib>:<key>=<value>`` of a log (the last wins)."""
    found = re.findall(r"^WEBKIT:W3:INFO:([\w-]+):([\w-]+)=(.*?)\s*$", serial, re.M)
    return {(lib, key): value for lib, key, value in found}


def failures(serial: str) -> list[str]:
    """Every failure the guest reported or suffered, trimmed."""
    found = re.findall(r"^(WEBKIT:W3:FAIL.*)$", serial, re.M)
    killed = re.findall(r"^(user: task \d+ killed by.*)$", serial, re.M)
    return [line.strip()[:200] for line in found + killed]


def judge(serial: str, host: str, expected_body: str | None = None,
          servers: list[str] | None = None) -> list[Verdict]:
    """Verdicts for one boot.

    `host` is the offline run on the build host; `expected_body` the digest
    prefix of the page the harness serves; `servers` the problems the harness
    found in what its servers saw (empty = fine, None = not checked).
    """
    verdicts = [Verdict("WEBKIT:W3:UP", has_line(serial, "WEBKIT:W3:UP"))]
    for lib in LIBS:
        verdicts.append(Verdict(f"WEBKIT:W3:PASS:{lib}", has_line(serial, f"WEBKIT:W3:PASS:{lib}")))
    verdicts.append(Verdict("WEBKIT:W3:PASS", has_line(serial, "WEBKIT:W3:PASS")))
    bad = failures(serial)
    verdicts.append(Verdict("no failure lines", not bad, "; ".join(bad)))

    guest, reference = info(serial), info(host)
    for lib, key in COMPARED:
        want, got = reference.get((lib, key)), guest.get((lib, key))
        name = f"{lib} {key} equals the host's"
        if want is None:
            verdicts.append(Verdict(name, False, "the host reference has no such value"))
        else:
            verdicts.append(Verdict(name, got == want, "" if got == want else f"guest {got}, host {want}"))
    if expected_body is not None:
        got = guest.get(("curl", "sha256"))
        verdicts.append(Verdict("curl fetched the page the server holds", got == expected_body,
                                "" if got == expected_body else f"got {got}, expected {expected_body}"))
    if servers is not None:
        verdicts.append(Verdict("the harness server saw the handshake and the request", not servers,
                                "; ".join(servers)))
    return verdicts


def passed(verdicts: list[Verdict]) -> bool:
    return all(v.ok for v in verdicts)
