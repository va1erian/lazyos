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
compute the same thing on LazyOS as on Linux. A value that says nothing
(empty, one repeated digit, a Skia surface where the line or the text inked no
pixel) never counts as agreement: two empty runs would otherwise be equal.

Both runs also print ``WEBKIT:W3:BIN:<sha256>``, the digest of their own
executable. The guest's must equal the host reference's (an image built from an
older ``depsproof`` is not what the reference describes), and the reference's
must equal the digest of ``target/webkit/bin/depsproof`` as the caller finds it
(``deps.py --only X`` after the reference was written is not either).
"""

from __future__ import annotations

import hashlib
import re
from dataclasses import dataclass
from pathlib import Path

#: The libraries that must pass, in the order the program checks them.
LIBS = ("zlib", "brotli", "icu", "libpng", "libjpeg-turbo", "libwebp", "libxml2", "expat", "sqlite",
        "freetype", "harfbuzz", "fontconfig", "skia", "libpsl", "openssl", "nghttp2", "curl")
#: INFO values the guest and the host must agree on.
COMPARED = (("zlib", "crc32"), ("libpng", "pixels"), ("libjpeg-turbo", "pixels"), ("libwebp", "pixels"),
            ("freetype", "glyph-A"), ("harfbuzz", "glyphs"), ("harfbuzz", "advance"),
            ("skia", "pixels"), ("skia", "inked"))
REBUILD = "run python tools/webkit/deps.py, then python tools/webkit/run.py without --no-build"


@dataclass(frozen=True)
class Verdict:
    name: str
    ok: bool
    detail: str = ""


def sha256_of(path: Path) -> str:
    """The SHA-256 of a file, as the program prints its own."""
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def has_line(serial: str, marker: str) -> bool:
    """Whether a whole line is `marker` (so `...PASS` is not `...PASS:zlib`)."""
    return re.search(rf"^{re.escape(marker)}\s*$", serial, re.M) is not None


def info(serial: str) -> dict[tuple[str, str], str]:
    """Every ``WEBKIT:W3:INFO:<lib>:<key>=<value>`` of a log (the last wins)."""
    found = re.findall(r"^WEBKIT:W3:INFO:([\w-]+):([\w-]+)=(.*?)\s*$", serial, re.M)
    return {(lib, key): value for lib, key, value in found}


def binary_digest(serial: str) -> str | None:
    """The ``WEBKIT:W3:BIN:<sha256>`` of a log, or None (also for ``unreadable``)."""
    found = re.findall(r"^WEBKIT:W3:BIN:([0-9a-f]{64})\s*$", serial, re.M)
    return found[-1] if found else None


def trivial(key: str, value: str) -> str:
    """Why an INFO value proves nothing, or ``""``: empty, a single repeated
    digit (``0``, ``00000000``), or an ``inked`` count (``<line>+<text>``)
    with a part that is zero or not a number."""
    value = value.strip()
    if not value:
        return "empty"
    if key == "inked":
        parts = value.split("+")
        if not all(part.isdigit() for part in parts) or any(int(part) == 0 for part in parts):
            return f"{value!r}: something was not drawn"
    # A zero of any length, or a digest (8 or more characters) of one repeated
    # character, proves nothing; a small count such as `glyphs=22` can
    # legitimately be one repeated digit.
    if key != "inked" and (set(value) == {"0"} or (len(value) >= 8 and len(set(value)) <= 1)):
        return f"{value!r} is a single repeated digit"
    return ""


def failures(serial: str) -> list[str]:
    """Every failure the guest reported or suffered, trimmed."""
    found = re.findall(r"^(WEBKIT:W3:FAIL.*)$", serial, re.M)
    killed = re.findall(r"^(user: task \d+ killed by.*)$", serial, re.M)
    return [line.strip()[:200] for line in found + killed]


def judge_binary(serial: str, host: str, binary: str | None) -> list[Verdict]:
    """Verdicts that the guest, the host reference and ``target/webkit/bin``
    are one build (`binary` is the SHA-256 of the file now, None = unchecked)."""
    guest, reference = binary_digest(serial), binary_digest(host)
    out = []
    if reference is None:
        out.append(Verdict("the host reference names its binary", False,
                           f"depsproof.host.txt has no WEBKIT:W3:BIN line ({REBUILD})"))
    elif binary is not None:
        same = reference == binary
        out.append(Verdict("the host reference is of target/webkit/bin/depsproof", same,
                           "" if same else f"reference {reference[:12]}, binary {binary[:12]}: {REBUILD}"))
    same = guest is not None and guest == reference
    detail = "" if same else (f"guest ran {(guest or 'an unknown binary')[:12]}, the reference is of "
                              f"{(reference or 'an unknown binary')[:12]}: {REBUILD}")
    out.append(Verdict("the guest ran the binary the host reference ran", same, detail))
    return out


def judge(serial: str, host: str, expected_body: str | None = None,
          servers: list[str] | None = None, binary: str | None = None) -> list[Verdict]:
    """Verdicts for one boot.

    `host` is the offline run on the build host; `expected_body` the digest
    prefix of the page the harness serves; `servers` the problems the harness
    found in what its servers saw (empty = fine, None = not checked); `binary`
    the SHA-256 of ``target/webkit/bin/depsproof`` as it is now (None = not
    checked).
    """
    verdicts = [Verdict("WEBKIT:W3:UP", has_line(serial, "WEBKIT:W3:UP"))]
    for lib in LIBS:
        verdicts.append(Verdict(f"WEBKIT:W3:PASS:{lib}", has_line(serial, f"WEBKIT:W3:PASS:{lib}")))
    verdicts.append(Verdict("WEBKIT:W3:PASS", has_line(serial, "WEBKIT:W3:PASS")))
    bad = failures(serial)
    verdicts.append(Verdict("no failure lines", not bad, "; ".join(bad)))

    verdicts.extend(judge_binary(serial, host, binary))
    guest, reference = info(serial), info(host)
    for lib, key in COMPARED:
        want, got = reference.get((lib, key)), guest.get((lib, key))
        name = f"{lib} {key} equals the host's"
        if want is None:
            verdicts.append(Verdict(name, False, "the host reference has no such value"))
        elif got != want:
            verdicts.append(Verdict(name, False, f"guest {got}, host {want}"))
        else:
            empty = trivial(key, want)
            verdicts.append(Verdict(name, not empty, f"both are trivial: {empty}" if empty else ""))
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
