"""Read the `lazyvm` fixture's lines (`tools/abi/fixtures/src/lazyvm.rs`) out of a serial log.

The fixture hex-encodes everything a command printed, so the bytes survive the serial
console unchanged (no CR/LF translation, no interleaving inside a line). `parse` rebuilds
each command's stdout, stderr and exit status and checks every chunk arrived.
"""

from __future__ import annotations

import re
from dataclasses import dataclass, field

MARKER = re.compile(r"LAZYVM:([A-Z]+)(?::(.*))?$")


@dataclass
class Result:
    """What one command did."""

    line: bytes = b""
    stdout: bytes = b""
    stderr: bytes = b""
    exit: int | None = None
    timed_out: bool = False
    #: A stream passed the fixture's cap (`CAP`, 8 MiB): the rest was dropped.
    truncated: bool = False
    spawn_error: str = ""
    #: The stdout and stderr lengths the END line announced.
    sent: tuple[int, int] | None = None
    chunks: dict[str, list[tuple[int, bytes]]] = field(
        default_factory=lambda: {"out": [], "err": []})


@dataclass
class Run:
    """Everything the fixture reported in one boot."""

    count: int | None = None
    done: bool = False
    failure: str = ""
    results: dict[int, Result] = field(default_factory=dict)
    errors: list[str] = field(default_factory=list)


def encode(timeout: int, argv: list[str]) -> str:
    """One line of the command list the fixture reads: the timeout, then each argument in hex."""
    return " ".join([str(timeout), *(arg.encode("utf-8").hex() for arg in argv)])


def parse(serial: str) -> Run:
    """Every `LAZYVM:` line of `serial`, assembled per command."""
    run = Run()
    for raw in serial.splitlines():
        match = MARKER.search(raw.rstrip("\r"))
        if not match:
            continue
        try:
            take(run, match.group(1), match.group(2) or "")
        except (ValueError, KeyError):
            run.errors.append(f"malformed line: {raw.strip()[:120]}")
    for n, result in run.results.items():
        finish(run, n, result)
    return run


def take(run: Run, kind: str, rest: str) -> None:
    """Record one marker."""
    if kind == "INIT":
        run.count = int(rest)
    elif kind == "DONE":
        run.done = True
    elif kind == "FAIL":
        run.failure = rest
    elif kind in ("CMD", "DATA", "SPAWN", "END"):
        n, _, rest = rest.partition(":")
        result = run.results.setdefault(int(n), Result())
        if kind == "CMD":
            result.line = bytes.fromhex(rest)
        elif kind == "SPAWN":
            result.spawn_error = rest
        elif kind == "DATA":
            stream, seq, data = rest.split(":")
            result.chunks[stream].append((int(seq), bytes.fromhex(data)))
        else:
            code, out_len, err_len, timed_out, truncated = rest.split(":")
            result.exit = int(code)
            result.timed_out = timed_out == "1"
            result.truncated = truncated == "1"
            result.sent = (int(out_len), int(err_len))


def finish(run: Run, n: int, result: Result) -> None:
    """Join the chunks in order and check none is missing."""
    for stream in ("out", "err"):
        chunks = sorted(result.chunks[stream])
        if [seq for seq, _ in chunks] != list(range(len(chunks))):
            run.errors.append(f"command {n}: std{stream} chunks out of order or missing")
        data = b"".join(chunk for _, chunk in chunks)
        setattr(result, "stdout" if stream == "out" else "stderr", data)
    got = (len(result.stdout), len(result.stderr))
    if result.sent is None:
        run.errors.append(f"command {n}: no END line")
    elif got != result.sent:
        run.errors.append(f"command {n}: got {got[0]}+{got[1]} bytes, "
                          f"the guest sent {result.sent[0]}+{result.sent[1]}")
