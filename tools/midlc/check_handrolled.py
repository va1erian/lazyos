#!/usr/bin/env python3
"""Count hand-rolled Messenger protocol code outside the generated crate.

docs/client-api-plan.md section 3.7 (stage C6). Every interface is a `.midl`
file and its client and server are generated; code that builds a `Header`,
scans a reply for the error field, dispatches on `METHOD_*` constants, names a
service in a string const or hashes names by hand is a private copy of the
protocol that drifts from the generated one. This lint reports a count per
file. CI runs `--ratchet`: the counts may only go down. A new file with a hit,
or a file whose count rises, fails; a file that improves must lower its
entry (`--update-baseline`, which never raises one), so saved headroom cannot
be spent on new hand-rolled code.

Patterns (one hit per occurrence):

    header       `Header { version: ...` literal (any other `Header` struct,
                 e.g. a GPT or ELF one, has no `version` first field)
    error-field  `Kind::Error` or `ERROR_FIELD`: a hand scan for the error
    arm          `METHOD_X =>` (or `METHOD_X | METHOD_Y =>`): hand dispatch
    service-name `const X: &str = "os.lazy.<...>"`
    fnv1a64      `fn fnv1a64` definition (the generated code carries the ids)
    reply-helper `fn error_reply`, or `fn parcel(.., method, ..)`

Not scanned (they are the implementation, or not clients):

    libs/generated/, libs/rhai-lazy/src/msg/idl.rs, libs/rhai-lazy/api/
                                  generated crate and the Rhai bindings
    libs/messenger/               the runtime (parcel, server, client)
    libs/lazyos-sys/              syscall layer (`client.rs`, `msg/parcel.rs`)
    user/src/messenger/client.rs  the user transport
    xui-app/src/platform/messenger.rs   the xui transport
    kernel/                       the IPC core and its suites
    any file starting with an `@generated` line
    test code: `tests/`, `fixtures/` and `benches/` directories, `tests.rs`,
    `*_tests.rs`, `test_*.rs`, `mock*.rs`, and everything from the first
    `#[cfg(test)]` line of a file on
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from collections import Counter
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
BASELINE = Path(__file__).with_name("handrolled_baseline.json")

EXCLUDED_PREFIXES = (
    "libs/generated/",
    "libs/rhai-lazy/src/msg/idl.rs",
    "libs/rhai-lazy/api/",
    "libs/messenger/",
    "libs/lazyos-sys/",
    "user/src/messenger/client.rs",
    "xui-app/src/platform/messenger.rs",
    "kernel/",
)
TEST_DIRS = frozenset({"tests", "fixtures", "benches"})
TEST_FILE = re.compile(r"(?:^tests\.rs|_tests?\.rs|^test_.*\.rs|^mock.*\.rs)$")

METHOD = r"(?:\w+::)*METHOD_[A-Z0-9_]+"
PATTERNS: dict[str, re.Pattern[str]] = {
    "header": re.compile(r"\bHeader\s*\{\s*version\s*:"),
    "error-field": re.compile(r"\bKind::Error\b|\bERROR_FIELD\b"),
    "arm": re.compile(rf"\b{METHOD}(?:\s*\|\s*{METHOD})*\s*(?:if\b[^=\n]*)?=>"),
    "service-name": re.compile(r'\bconst\s+\w+\s*:\s*&(?:\'static\s+)?str\s*=\s*"os\.lazy\.'),
    "fnv1a64": re.compile(r"\bfn\s+fnv1a64\b"),
    "reply-helper": re.compile(r"\bfn\s+error_reply\b|\bfn\s+parcel\s*\([^)]*\bmethod\b"),
}


def tracked_rust_files(root: Path) -> list[str]:
    out = subprocess.run(
        ["git", "ls-files", "--cached", "--others", "--exclude-standard", "*.rs"],
        cwd=root,
        capture_output=True,
        text=True,
        check=True,
    ).stdout
    return [line for line in out.splitlines() if line]


def is_excluded(path: str) -> bool:
    if path.startswith(EXCLUDED_PREFIXES) or "/target/" in path:
        return True
    parts = path.split("/")
    return bool(TEST_DIRS & set(parts[:-1])) or bool(TEST_FILE.search(parts[-1]))


def production_text(text: str) -> str | None:
    """The text a scan covers: none for a generated file, and a file's
    `#[cfg(test)]` tail is dropped (tests come last by convention)."""
    if "@generated" in text[:300]:
        return None
    marker = re.search(r"^\s*#\[cfg\(test\)\]", text, re.MULTILINE)
    return text[: marker.start()] if marker else text


def scan_text(text: str) -> list[tuple[int, str]]:
    """Hits of one file's text as `(line, pattern)`, in line order."""
    hits = []
    for name, pattern in PATTERNS.items():
        hits += [(text.count("\n", 0, m.start()) + 1, name) for m in pattern.finditer(text)]
    return sorted(hits)


def scan(root: Path) -> dict[str, list[tuple[int, str]]]:
    result: dict[str, list[tuple[int, str]]] = {}
    for path in tracked_rust_files(root):
        file = root / path
        if is_excluded(path) or not file.is_file():
            continue
        text = production_text(file.read_text(encoding="utf-8", errors="replace"))
        hits = scan_text(text) if text else []
        if hits:
            result[path] = hits
    return result


FIX = (
    "use the generated client/server (`libs/generated`, `idl/*.midl`, "
    "docs/client-api-plan.md section 3) instead of hand-rolling the protocol"
)


def ratchet(counts: dict[str, int], baseline: dict[str, int]) -> list[str]:
    """Files with more hits than their baseline (or any hit, when absent) and
    files that improved without lowering their entry."""
    problems = []
    for path in sorted(set(counts) | set(baseline)):
        now, was = counts.get(path, 0), baseline.get(path, 0)
        if now > was:
            problems.append(f"{path}: {now} hand-rolled Messenger hit(s), baseline {was}: {FIX}")
        elif now < was:
            problems.append(
                f"{path}: {now} hit(s), baseline {was}: lower it with "
                "`python tools/midlc/check_handrolled.py --update-baseline`"
            )
    return problems


def lowered(counts: dict[str, int], baseline: dict[str, int]) -> dict[str, int]:
    """The baseline with every entry at most its current count; zeroed files
    drop out and files not in the baseline are never added."""
    out = {p: min(n, counts.get(p, 0)) for p, n in baseline.items()}
    return {p: n for p, n in sorted(out.items()) if n}


def write_json(path: Path, counts: dict[str, int]) -> None:
    path.write_text(json.dumps(counts, indent=1) + "\n", encoding="utf-8")


def report(found: dict[str, list[tuple[int, str]]], verbose: bool) -> None:
    by_pattern: Counter[str] = Counter()
    for path in sorted(found, key=lambda p: (-len(found[p]), p)):
        print(f"{len(found[path]):4d}  {path}")
        for line, name in found[path]:
            by_pattern[name] += 1
            if verbose:
                print(f"        {line}: {name}")
    print()
    for name in PATTERNS:
        print(f"{by_pattern[name]:4d}  {name}")
    print(f"{sum(by_pattern.values()):4d}  total, {len(found)} files")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("--ratchet", action="store_true", help="fail when a file exceeds its baseline")
    parser.add_argument(
        "--update-baseline",
        action="store_true",
        help=f"lower {BASELINE.name} to the tree (never raises, drops zeroed files)",
    )
    parser.add_argument(
        "--init-baseline", action="store_true", help=f"write {BASELINE.name} from scratch"
    )
    parser.add_argument("--baseline", type=Path, default=BASELINE, help=argparse.SUPPRESS)
    parser.add_argument("--root", type=Path, default=ROOT)
    parser.add_argument("--verbose", "-v", action="store_true", help="list every hit")
    args = parser.parse_args(argv)

    found = scan(args.root)
    report(found, args.verbose)
    counts = {path: len(hits) for path, hits in found.items()}
    saved = json.loads(args.baseline.read_text(encoding="utf-8")) if args.baseline.is_file() else {}

    if args.init_baseline:
        write_json(args.baseline, dict(sorted(counts.items())))
        print(f"wrote {args.baseline.name}: {sum(counts.values())} hits in {len(counts)} files")
        return 0
    if args.update_baseline:
        new = lowered(counts, saved)
        write_json(args.baseline, new)
        print(f"wrote {args.baseline.name}: {sum(new.values())} hits in {len(new)} files")
        return 0
    if args.ratchet:
        problems = ratchet(counts, saved)
        for problem in problems:
            print(f"RATCHET  {problem}", file=sys.stderr)
        if problems:
            print(
                "check_handrolled: Messenger protocol code is generated, not hand-written",
                file=sys.stderr,
            )
            return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
