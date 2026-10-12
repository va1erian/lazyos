"""Serving a file with `Content-Length` and `Range` (from the Q0 spike's `serve.py`)."""

from __future__ import annotations

import re
from pathlib import Path

CHUNK = 256 << 10
_RANGE = re.compile(r"^bytes=(\d*)-(\d*)$")


class RangeError(Exception):
    """An unsatisfiable `Range` (answered 416)."""


def parse_range(header: str | None, size: int) -> tuple[int, int] | None:
    """The inclusive `(first, last)` a `Range` header asks of a `size`-byte file.

    `None` for no header or one we ignore (several ranges, other units, bad
    syntax: RFC 9110 says to answer 200 with the whole file). `RangeError`
    when the single range lies beyond the end.
    """
    match = _RANGE.match((header or "").strip())
    if not match or (match[1] == "" and match[2] == ""):
        return None
    first, last = match[1], match[2]
    if first == "":  # suffix: the last N bytes
        n = int(last)
        if n == 0 or size == 0:
            raise RangeError
        return max(size - n, 0), size - 1
    start = int(first)
    if last != "" and int(last) < start:
        return None  # an invalid range is ignored, not refused
    if start >= size:
        raise RangeError
    return start, size - 1 if last == "" else min(int(last), size - 1)


def plan(path: Path, range_header: str | None) -> tuple[int, dict, int, int]:
    """`(status, headers, first byte, length)` of the answer for a file.

    Raises `RangeError`; the caller answers 416 with `Content-Range: bytes */size`.
    """
    size = path.stat().st_size
    span = parse_range(range_header, size)
    first, last = span if span else (0, size - 1)
    length = last - first + 1 if size else 0
    headers = {"Content-Type": "application/octet-stream", "Accept-Ranges": "bytes",
               "Content-Length": str(length)}
    if span:
        headers["Content-Range"] = f"bytes {first}-{last}/{size}"
    return (206 if span else 200), headers, first, length


def copy(path: Path, first: int, length: int, out) -> int:
    """Write `length` bytes of `path` from `first` to `out`; return how many were sent.

    A client that goes away raises `ConnectionError`/`TimeoutError` to the caller.
    """
    sent = 0
    with open(path, "rb") as f:
        f.seek(first)
        while sent < length:
            chunk = f.read(min(CHUNK, length - sent))
            if not chunk:
                break
            out.write(chunk)
            sent += len(chunk)
    return sent
