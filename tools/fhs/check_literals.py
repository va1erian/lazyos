#!/usr/bin/env python3
"""Fail when a well-known path or boot-volume name is written as a literal.

Every such name lives in `libs/fhs` (docs/architecture/filesystem.md, "Paths").
A Rust string literal outside that crate that starts with a well-known
directory, or that names an old 8.3 boot-volume file, is reported. Byte
strings count too: a spawn line (`b"/system/bin/beep role=intruder\0"`) names
a program and must compose it from `fhs::bin`. Comments, test code, generated
files and `target/` are skipped; a deliberate exception goes in
`tools/fhs/allowlist.txt` as one `path:reason` line (the whole file) or
`path:LINE:reason` (one line).

    python tools/fhs/check_literals.py [--root DIR]
"""
import argparse
import re
import sys
from pathlib import Path

PREFIXES = ("/data", "/docs", "/home", "/conf", "/accounts", "/apps", "/logs",
            "/system", "/transient", "/etc")
# The flat 8.3 names of the F2 image root: none may come back (F3).
BOOT_NAME = re.compile(r"\b[A-Z0-9]{1,8}\.(ELF|LST|TYP|LZP)\b|\b(PASSWD|BUSYBOX)\b")

# `'x'`, `'\n'`, `'\x41'`, `'\u{1F600}'`; a lifetime (`'a`) has no closing quote.
CHAR_BODY = r"(?:\\(?:u\{[0-9a-fA-F_]+\}|x[0-9a-fA-F]{2}|.)|[^'\\])"
CHAR_LITERAL = re.compile("'" + CHAR_BODY + "'")
# Comments and literals, to blank out before counting braces.
MASKABLE = re.compile(
    r'//[^\n]*|/\*.*?\*/|b?r#*".*?"#*|b?"(?:\\.|[^"\\])*"|' + CHAR_LITERAL.pattern, re.S
)

SKIP_DIRS = {"target", ".git", ".claude", "shots", "node_modules", "fhs", "generated"}
SKIP_FILES = {"libs/rhai-lazy/src/msg/idl.rs"}
SKIP_TREES = ("libs/generated/", "libs/fhs/", "tools/abi/fixtures/", "kernel/src/tests/", "fuzz/")


def literals(src):
    """Yield (line, text) for each string literal outside comments."""
    i, n, line = 0, len(src), 1
    while i < n:
        c = src[i]
        if c == "\n":
            line += 1
            i += 1
        elif src.startswith("//", i):
            while i < n and src[i] != "\n":
                i += 1
        elif src.startswith("/*", i):
            depth = 1
            i += 2
            while i < n and depth:
                if src.startswith("/*", i):
                    depth += 1
                    i += 2
                elif src.startswith("*/", i):
                    depth -= 1
                    i += 2
                else:
                    line += src[i] == "\n"
                    i += 1
        elif c == "r" and re.match(r'r#*"', src[i:i + 8]) and not (i and (src[i - 1].isalnum() or src[i - 1] == "_")):
            hashes = len(re.match(r"r(#*)", src[i:]).group(1))
            start = i + 2 + hashes
            end = src.find('"' + "#" * hashes, start)
            end = n if end < 0 else end
            yield line, src[start:end]
            line += src.count("\n", i, end)
            i = end + 1 + hashes
        elif c == '"':
            start, j = i + 1, i + 1
            while j < n and src[j] != '"':
                j += 2 if src[j] == "\\" else 1
            yield line, src[start:j]
            line += src.count("\n", i, j)
            i = j + 1
        elif c == "'":
            m = CHAR_LITERAL.match(src, i)
            i = m.end() if m else i + 1  # a char literal such as '"', or a lifetime
        else:
            i += 1



# `/tmp` itself and anything below it, but not `/tmpfs` or `/tmp2`.
TMP = re.compile(r"/tmp(?:/|$)")


def offends(text):
    return text.startswith(PREFIXES) or bool(TMP.match(text)) or bool(BOOT_NAME.search(text))


def is_test_file(rel):
    name = rel.rsplit("/", 1)[-1]
    return "/tests/" in "/" + rel or name in ("tests.rs", "test.rs") or name.endswith("_tests.rs")


def test_ranges(src):
    """(first, last) lines of each inline `#[cfg(test)] mod { .. }`.

    Braces are counted on a copy with comments and literals blanked, so a brace
    in a string cannot end the module early, and code after the module is
    scanned like any other.
    """
    masked = MASKABLE.sub(lambda m: re.sub(r"[^\n]", " ", m.group()), src)
    ranges = []
    for m in re.finditer(r"^\s*#\[cfg\(test\)\]\s*\n\s*(?:pub\s+)?mod\b[^{;]*\{", masked, re.M):
        depth, end = 1, m.end()
        while end < len(masked) and depth:
            depth += {"{": 1, "}": -1}.get(masked[end], 0)
            end += 1
        ranges.append((src.count("\n", 0, m.start()) + 1, src.count("\n", 0, end) + 1))
    return ranges


ENTRY = re.compile(r"^(?P<path>[^:]+):(?:(?P<line>\d+):)?(?P<reason>.*)$")


def load_allowlist(path):
    """`path:reason` (whole file) and `path:LINE:reason` (one line) entries.

    A malformed entry, such as a line entry with no reason, is an error: it must
    not quietly widen into a whole-file exemption.
    """
    whole, lines = {}, {}
    if path.exists():
        for number, raw in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
            raw = raw.strip()
            if not raw or raw.startswith("#"):
                continue
            m = ENTRY.match(raw)
            # `path:2:` is a line entry that lost its reason, never `path` + "2:".
            if not m or not m.group("reason").strip() or re.fullmatch(r"\d+:?", m.group("reason").strip()):
                raise ValueError(f"{path}:{number}: malformed allowlist entry {raw!r}")
            if m.group("line"):
                lines[(m.group("path"), int(m.group("line")))] = m.group("reason")
            else:
                whole[m.group("path")] = m.group("reason")
    return whole, lines


def scan(root, allowlist):
    whole, lines = allowlist
    found = []
    for path in sorted(root.rglob("*.rs")):
        rel = path.relative_to(root).as_posix()
        parts = rel.split("/")
        if (SKIP_DIRS & set(parts[:-1]) or rel in SKIP_FILES or rel.startswith(SKIP_TREES)
                or is_test_file(rel) or rel in whole):
            continue
        src = path.read_text(encoding="utf-8", errors="replace")
        tests = test_ranges(src)
        for line, text in literals(src):
            if any(first <= line <= last for first, last in tests):
                continue
            if offends(text) and (rel, line) not in lines:
                found.append((rel, line, text))
    return found


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--root", default=Path(__file__).resolve().parents[2], type=Path)
    ap.add_argument("--allowlist", type=Path)
    args = ap.parse_args(argv)
    try:
        allow = load_allowlist(args.allowlist or args.root / "tools/fhs/allowlist.txt")
    except ValueError as error:
        print(error, file=sys.stderr)
        return 2
    found = scan(args.root, allow)
    for rel, line, text in found:
        print(f"{rel}:{line}: path literal {text!r}: use libs/fhs")
    if found:
        print(f"{len(found)} literal(s); see docs/architecture/filesystem.md (Paths)")
    return 1 if found else 0


if __name__ == "__main__":
    sys.exit(main())
