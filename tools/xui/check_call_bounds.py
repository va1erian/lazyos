#!/usr/bin/env python3
"""Reject unbounded Messenger calls on UI threads (issue #720).

Every `Service::call`/`call_detailed` (and the display client's calls) in
`xui-app` carries a PIT-tick deadline: a call without one parks the UI
thread until the service answers, and a window cannot paint while it waits.
The unbounded shape exists only for the tree's worker threads as the
explicit `call_unbounded`/`call_detailed_unbounded`; a new user of either
must be justified in `ALLOWLIST`, and the deadline-less literal forms
(`call_until(x, 0)`, a zero-deadline `msg_call`) are refused outright.

    python tools/xui/check_call_bounds.py [--root DIR]

`tools/xui/test_check_call_bounds.py` tests the scan; the `lint` job of
`.github/workflows/ci.yml` runs it.
"""

import argparse
import re
from dataclasses import dataclass
from pathlib import Path

# `xui-app` lives here; the scan covers every crate in the workspace that
# issues Messenger calls (the whole tree, comments aside).
APP_DIR = "xui-app"

# Files where `call_unbounded`/`call_detailed_unbounded` is allowed, a
# directory (trailing `/`) or file path (relative to the root, `/`-separated)
# -> why. Empty today: every call site of the #716 worker conversion is
# bounded. An unbounded path is added only when a worker thread (never the
# UI loop) genuinely needs it, and the job says so in one line.
ALLOWLIST: dict[str, str] = {
    # The API's definitions (and their delegation bodies); consumers are
    # what the scan polices.
    "xui-app/src/platform/messenger.rs": "the wrappers themselves",
}

# The explicit deadline-less forms (`platform/messenger.rs` is where they
# are defined; the scan skips `platform/messenger.rs`'s own bodies of
# course, which define them).
UNBOUNDED_NAMES = ("call_unbounded", "call_detailed_unbounded")

# `call_until(parcel, 0)` is the deadline-less literal the API used to
# allow on the display client; a caller passes a tick deadline instead.
LITERAL_DEADLINE_ZERO = [
    r"\.call_until\([^,\n]+,\s*0\s*\)",
    r"\.msg_call\(\s*[^,]+,[^,]+,[^,]+,\s*0\s*\)",
]


@dataclass(frozen=True)
class Hit:
    path: str
    line: int
    text: str
    why: str


def scan(root: Path) -> list[Hit]:
    """Every offending site under `root/xui-app`, comments stripped."""
    hits: list[Hit] = []
    app = root / APP_DIR
    if not app.is_dir():
        return hits
    literals = [re.compile(p) for p in LITERAL_DEADLINE_ZERO]
    for file in sorted(app.rglob("*.rs")):
        rel = file.relative_to(root).as_posix()
        for line_no, line in enumerate(
            strip_comments(file.read_text(encoding="utf-8", errors="replace").splitlines()),
            1,
        ):
            text = line.strip()
            if not text or text.startswith("//"):
                continue
            found = next((n for n in UNBOUNDED_NAMES if f".{n}(" in text), None)
            if found is not None and allowed(rel) is None:
                hits.append(Hit(rel, line_no, text, f".{found} without an allowlist entry"))
            if any(literal.search(text) for literal in literals):
                hits.append(Hit(rel, line_no, text, "a deadline-less literal (0 is for ever)"))
    return hits


def allowed(rel: str) -> str | None:
    """The most specific allowlist entry that covers `rel`, if any."""
    best = None
    for path, reason in ALLOWLIST.items():
        prefix = path.rstrip("/")
        if rel == prefix or rel.startswith(prefix + "/"):
            if best is None or len(prefix) > len(best[0]):
                best = (prefix, reason)
    return None if best is None else best[1]


def strip_comments(lines: list[str]) -> list[str]:
    """Blank `//` line comments and the `/* */` ones, so a documented
    refusal does not look like a call site."""
    out = []
    deep = 0
    for text in lines:
        i, kept = 0, []
        while i < len(text):
            if deep:
                end = text.find("*/", i)
                if end < 0:
                    i = len(text)
                    continue
                deep -= 1
                i = end + 2
                continue
            if text.startswith("//", i):
                break
            if text.startswith("/*", i):
                deep += 1
                i += 2
                continue
            kept.append(text[i])
            i += 1
        out.append("".join(kept))
    return out


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path.cwd())
    args = parser.parse_args()
    root = args.root.resolve()

    hits = scan(root)
    # Each allowlist entry must still cover a file (a moved one is dead).
    existing = {f.relative_to(root).as_posix() for f in (root / APP_DIR).rglob("*.rs")}
    for path in ALLOWLIST:
        if not any(allowed(rel) == ALLOWLIST[path] for rel in existing if allowed(rel)):
            print(f"stale allowlist entry: {path}")
            hits.append(Hit(path, 0, "", "the allowlisted file is gone"))
    for hit in hits:
        print(f"{hit.path}:{hit.line}: {hit.why}")
        print(f"    {hit.text}")
    if hits:
        print(
            "A Messenger call on a UI thread has no deadline (issue #720): "
            "bound it (`Service::call(..., ticks)`) or move it to a worker "
            "thread; `call_unbounded` needs an ALLOWLIST entry with a reason."
        )
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
