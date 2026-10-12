#!/usr/bin/env python3
"""Find ad hoc trace lines: a print whose text starts with `UPPER:`.

docs/tracing-plan.md (stage T0). Test evidence, UI markers and the like must
go through the `trace` API (`libs/tracefmt` and its three sinks), never a
print; this lint is the progress bar of that migration. It reports a count
per file (and per tree), and `--enforce` fails on any hit, which CI turns on
at T4.

A hit is a literal that starts with a capital letter and has a colon:

    serial_println!("UP:...")   serial_print!("UP:...")
    println!("UP:...")          eprintln!("UP:...")
    write_str(&format!("UP:...")

Not hits: the `libs/tracefmt` crate and the three sink modules (they are the
one place a line is written), the kernel suite (`kernel/src/tests/`, whose
`TEST:` protocol goes through the kernel sink at T1 but keeps its own
switch), the Linux ABI fixtures (the programs under test) and the class-2
diagnostics of the plan's section 3, which stay on `serial_println!` and the
rings in every image.
"""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
from collections import Counter
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]

# One print macro (or the services' write) followed by a format string that
# starts with a marker: `UPPER` then a colon.
CALL = re.compile(
    r"""(?:\bserial_println!|\bserial_print!|\beprintln!|\bprintln!|\beprint!|\bprint!
        |write_str\(\s*&format!)\(\s*"(?P<tag>[A-Z][A-Z0-9_]*:)""",
    re.VERBOSE,
)

# Trees of the inventory table in docs/tracing-plan.md section 1.
TREES = [
    ("kernel", "kernel/"),
    ("user", "user/"),
    ("xui-app", "xui-app/"),
    ("lazyrad-os", "lazyrad-os/"),
    ("doom", "doom/"),
    ("emusic", "emusic/"),
    ("quake", "quake/"),
    ("srb2kart", "srb2kart/"),
]

# Not scanned: the one place a line is written, the kernel suite and the
# programs under test.
EXCLUDED_PREFIXES = (
    "libs/tracefmt/",
    "kernel/src/trace.rs",
    "user/src/trace.rs",
    "xui-app/src/trace.rs",
    "xui-app/src/trace/",
    "kernel/src/tests/",
    "tools/abi/fixtures/",
)

# Class 2 (docs/tracing-plan.md section 3): diagnostics an operator needs in
# a normal image. They stay on `serial_println!` and `sys::write_str`.
DIAGNOSTIC_PREFIXES = frozenset(
    {
        "EXCEPTION:",
        "HW:",
        "IRQOFF:",
        "DBGD:",
        "USBD:DUMP:",
        "PANIC:",
        # Data, not evidence: a JSON object the MCP bridge parses, longer
        # than a trace line may be.
        "MCP:FABRIC_STATS:",
        "MCP:TASK_SNAPSHOT:",
    }
)

# Channels whose arguments must never carry a secret (plan section 7).
SECRET_CHANNELS = ("LOGIN:", "GREETER:", "KEYD:", "ELEVD:", "SMB:", "ACCOUNTS:")
# Lower-case identifiers only: `KEYD_SECRETS` is a path constant, not a value.
# An underscore separates words (`api_token`); upper case never matches.
SECRET_WORDS = re.compile(r"(?<![A-Za-z0-9])(?:password|secret|token)s?(?![A-Za-z0-9])")
# `{password}` in a format string captures a variable of that name.
NAMED_CAPTURE = re.compile(r"(?<!\{)\{([A-Za-z_][A-Za-z0-9_]*)")


def tracked_rust_files(root: Path) -> list[str]:
    out = subprocess.run(
        ["git", "ls-files", "--cached", "--others", "--exclude-standard", "*.rs"], cwd=root, capture_output=True, text=True, check=True
    ).stdout
    return [line for line in out.splitlines() if line]


def tree_of(path: str) -> str:
    for name, prefix in TREES:
        if path.startswith(prefix):
            return name
    return "other"


def is_diagnostic(tag: str, text: str) -> bool:
    return any(text.startswith(prefix) for prefix in DIAGNOSTIC_PREFIXES) or tag in (
        "EXCEPTION:",
        "HW:",
    )


def scan_text(text: str) -> list[tuple[int, str, str]]:
    """Hits of one file's text as `(line, tag, literal-start)`."""
    hits = []
    for match in CALL.finditer(text):
        line = text.count("\n", 0, match.start()) + 1
        start = match.end() - len(match.group("tag"))
        literal = text[start : start + 80].split('"')[0]
        if is_diagnostic(match.group("tag"), literal):
            continue
        hits.append((line, match.group("tag"), literal))
    return hits


def call_args(text: str, at: int) -> str:
    """The code of the call whose format string starts at `at`: string
    literals blanked, up to the parenthesis that closes the call."""
    depth = 1
    out = []
    i = at
    while i < len(text) and depth:
        c = text[i]
        if c == '"':
            i += 1
            begin = i
            while i < len(text) and text[i] != '"':
                i += 2 if text[i] == "\\" else 1
            # The text is blanked, but a `{name}` capture is code.
            out.extend(f" {name} " for name in NAMED_CAPTURE.findall(text[begin:i]))
        elif c in "([{":
            depth += 1
        elif c in ")]}":
            depth -= 1
        else:
            out.append(c)
        i += 1
    return "".join(out)


def secret_hits(text: str) -> list[tuple[int, str]]:
    """Hits on a secret-bearing channel whose arguments mention a secret word.

    Only the code of the call counts: the words inside the format string are
    the line's own text (`ACCOUNTS:PASSWORD:PASS`), not a value.
    """
    found = []
    for match in CALL.finditer(text):
        tag = match.group("tag")
        if not tag.startswith(SECRET_CHANNELS):
            continue
        if SECRET_WORDS.search(call_args(text, match.start("tag") - 1)):
            found.append((text.count("\n", 0, match.start()) + 1, tag))
    return found


def scan(root: Path) -> dict[str, list[tuple[int, str, str]]]:
    result: dict[str, list[tuple[int, str, str]]] = {}
    for path in tracked_rust_files(root):
        if path.startswith(EXCLUDED_PREFIXES) or "/target/" in path:
            continue
        file = root / path
        if not file.is_file():
            continue
        hits = scan_text(file.read_text(encoding="utf-8", errors="replace"))
        if hits:
            result[path] = hits
    return result


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("--enforce", action="store_true", help="fail on any hit")
    parser.add_argument("--root", type=Path, default=ROOT)
    parser.add_argument("--verbose", "-v", action="store_true", help="list every hit")
    args = parser.parse_args(argv)

    found = scan(args.root)
    by_tree: Counter[str] = Counter()
    by_tag: Counter[str] = Counter()
    for path, hits in found.items():
        by_tree[tree_of(path)] += len(hits)
        for _, tag, _ in hits:
            by_tag[tag] += 1

    for path in sorted(found, key=lambda p: (-len(found[p]), p)):
        print(f"{len(found[path]):4d}  {path}")
        if args.verbose:
            for line, _, literal in found[path]:
                print(f"        {line}: {literal}")
    print()
    for name, _ in TREES + [("other", "")]:
        if by_tree[name]:
            print(f"{by_tree[name]:4d}  tree {name}")
    total = sum(by_tree.values())
    print(f"{total:4d}  total, {len(found)} files, {len(by_tag)} prefixes")

    leaks = []
    for path in found:
        text = (args.root / path).read_text(encoding="utf-8", errors="replace")
        leaks += [(path, line, tag) for line, tag in secret_hits(text)]
    for path, line, tag in leaks:
        print(f"SECRET?  {path}:{line}: {tag} mentions password/secret/token")

    if args.enforce and (total or leaks):
        print("check_adhoc: markers go through `trace`, never a print", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
