#!/usr/bin/env python3
"""Reject unbounded Messenger calls on UI threads (issue #720).

Every Messenger call in `xui-app` and `lazyrad-os` is bounded: a call without
a bound parks the UI thread until the service answers, and a window cannot
paint while it waits. A call goes through a generated client
(`messenger_generated::<interface>::Client`), which waits the `deadline` its
`.midl` resolves for the method (its own, else the interface `default`;
`midlc` refuses a method that resolves none). So this scan refuses:

* a call of a generated method whose `.midl` resolves `deadline=never` (the
  call would park the UI thread for ever): call `<method>_within(.., Deadline)`
  with a bound, or move the call to a worker thread;
* the unbounded shape, an explicit `Deadline::Never`, outside `ALLOWLIST`
  (worker threads and begun, non-parking calls, each with its reason);
* the deadline-less literal forms (`call_until(x, 0)`, a zero-deadline
  `msg_call`).

A file is checked against the interfaces it imports
(`use messenger_generated::os_lazy_foo_v1 as wire;`), so a method name that
another type also has is only a candidate in a file that uses that interface.

    python tools/xui/check_call_bounds.py [--root DIR]

`tools/xui/test_check_call_bounds.py` tests the scan; the `lint` job of
`.github/workflows/ci.yml` runs it.
"""

import argparse
import re
from dataclasses import dataclass
from pathlib import Path

# `xui-app` lives here; the scan covers every crate in the workspace that
# issues Messenger calls (the whole tree, comments aside). `lazyrad-os` is
# the other standalone workspace whose programs call services on a UI thread.
APP_DIR = "xui-app"
CALL_DIRS = (APP_DIR, "lazyrad-os")
IDL_DIR = "idl"

# Files where `Deadline::Never` is allowed, a
# directory (trailing `/`) or file path (relative to the root, `/`-separated)
# -> why. The one entry below is the deliberate case so far: an unbounded
# path is added only when a worker thread (never the UI loop) genuinely
# needs it, and the reason says what keeps it off the UI thread.
ALLOWLIST: dict[str, str] = {
    # The API's definitions (and their delegation bodies); consumers are
    # what the scan polices.
    "xui-app/src/platform/messenger.rs": "the wrappers themselves",
    # The install is begun with `messenger::call_async`, which never parks the
    # UI loop: `pkgd` unpacks at its own pace and the callback gets the answer.
    "xui-app/src/platform/pkg.rs": "the install's non-parking `call_async`",
    # The one request site inside `request` goes out `Never` only from a
    # thread the elevated machinery marked (`mark_worker`); a UI thread keeps
    # the `.midl` deadline. The marker's default is pinned by a unit test in
    # that module.
    "xui-app/src/platform/elevd.rs": "a worker-marked thread's elevd requests",
    # webd's self-test client runs on a thread of the webd process (no UI loop);
    # its own helpers share the generated names, and its service calls state
    # their bound with `_within`.
    "xui-app/webd/src/selftest.rs": "webd's self-test thread",
    # The two raw `lazyos_sys::msg::call` sites. The topics subscriber's takes
    # the method's generated `METHOD_*_DEADLINE` (or a poll) from its caller
    # and refuses `Deadline::Never`; the Fabric panel's `ListTopics` is bounded
    # by a constant.
    "xui-app/src/fabric/subscribe/transport.rs": "the topics subscriber's transport",
    "xui-app/src/fabric/broker.rs": "the Fabric panel's bounded ListTopics",
}

# The explicit deadline-less form.
UNBOUNDED_MARKS = ("Deadline::Never",)

# A raw kernel call states its deadline by hand, outside the generated
# client that resolves it from the `.midl`: only the allowlisted files do.
RAW_CALL = re.compile(r"\bmsg::call\(")

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


# `use messenger_generated::os_lazy_foo_v1 as wire;` (the alias is optional).
IMPORT = re.compile(r"messenger_generated::(os_[a-z0-9_]+)(?:\s+as\s+(\w+))?\s*;")
# Files that only exercise generated code: not a UI thread's calls.
TEST_FILE = re.compile(r"(^|/)(tests?|testkit)(/|\.rs$)|_tests?\.rs$|(^|/)fuzz\.rs$|test_support\.rs$")
# The constructors a generated client owns: a method with one of these names
# is generated as `<name>_call`.
RESERVED = {"connect", "try_connect", "from_connection", "connection"}


def snake(name: str) -> str:
    """The generated Rust name of an IDL method (`ListApps` -> `list_apps`)."""
    out = re.sub(r"(?<!^)(?=[A-Z])", "_", name).lower()
    return f"{out}_call" if out in RESERVED else out


def never_methods(root: Path) -> dict[str, set[str]]:
    """Per generated module (`os_lazy_foo_v1`), the names of its request/reply
    methods whose `.midl` resolves `deadline=never`: the method's own, or the
    interface's `default deadline=never;` when the method states none. A
    `oneway` method has no wait to bound."""
    found: dict[str, set[str]] = {}
    for file in sorted((root / IDL_DIR).glob("*.midl")):
        text = "\n".join(strip_comments(file.read_text(encoding="utf-8").splitlines()))
        for iface in re.finditer(r"^interface\s+([\w.]+)\s*\{", text, re.M):
            end = text.find("\n}", iface.end())
            body = text[iface.end() : end if end >= 0 else len(text)]
            default = re.search(r"^\s*default\b([^;]*);", body, re.M)
            default_never = default is not None and re.search(r"deadline=never\b", default.group(1)) is not None
            names = set()
            for method in re.finditer(r"^\s*method\s+(\w+)\(", body, re.M):
                stop = body.index(";", method.end())
                declaration = body[method.start() : stop]
                if re.search(r"(?<![A-Za-z_])oneway(?![A-Za-z_])", declaration):
                    continue
                own = re.search(r"deadline=(\w+)", declaration)
                if (own.group(1) == "never") if own else default_never:
                    names.add(snake(method.group(1)))
            found[iface.group(1).replace(".", "_")] = names
    return found


def scan(root: Path) -> list[Hit]:
    """Every offending site under the call trees of `root`, comments stripped."""
    hits: list[Hit] = []
    literals = [re.compile(p) for p in LITERAL_DEADLINE_ZERO]
    unbounded = never_methods(root)
    for tree in CALL_DIRS:
        base = root / tree
        if not base.is_dir():
            continue
        for file in sorted(base.rglob("*.rs")):
            if "target" in file.relative_to(base).parts:
                continue
            rel = file.relative_to(root).as_posix()
            lines = strip_comments(file.read_text(encoding="utf-8", errors="replace").splitlines())
            hits.extend(scan_file(rel, lines, literals, unbounded, tree == APP_DIR))
    return hits


def scan_file(
    rel: str,
    lines: list[str],
    literals: list[re.Pattern],
    unbounded: dict[str, set[str]],
    ui_tree: bool,
) -> list[Hit]:
    """The offending lines of one file."""
    hits: list[Hit] = []
    source = "\n".join(lines)
    # The interfaces this file builds a client of (`wire::Client::connect`).
    imported = {
        m.group(1)
        for m in IMPORT.finditer(source)
        if re.search(r"\b%s::Client\b" % re.escape(m.group(2) or m.group(1)), source)
    }
    candidates = {} if TEST_FILE.search(rel) else {
        name: mod for mod in imported for name in unbounded.get(mod, ())
    }
    call = re.compile(r"\.(%s)\(" % "|".join(map(re.escape, sorted(candidates)))) if candidates else None
    for line_no, line in enumerate(lines, 1):
        text = line.strip()
        if not text or text.startswith("//"):
            continue
        found = next((n for n in UNBOUNDED_MARKS if n in text), None)
        if ui_tree and found is not None and allowed(rel) is None:
            hits.append(Hit(rel, line_no, text, f"{found} without an allowlist entry"))
        if ui_tree and RAW_CALL.search(text) and allowed(rel) is None:
            hits.append(Hit(rel, line_no, text, "a raw `msg::call`: use a generated client"))
        if any(literal.search(text) for literal in literals):
            hits.append(Hit(rel, line_no, text, "a deadline-less literal (0 is for ever)"))
        if call is not None and allowed(rel) is None:
            for method in call.finditer(text):
                hits.append(
                    Hit(
                        rel,
                        line_no,
                        text,
                        f"`{method.group(1)}` of {candidates[method.group(1)]} is `deadline=never` "
                        "in its .midl (it would park the UI thread for ever)",
                    )
                )
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
    existing = {
        f.relative_to(root).as_posix() for tree in CALL_DIRS for f in (root / tree).rglob("*.rs")
    }
    for path in ALLOWLIST:
        if not any(allowed(rel) == ALLOWLIST[path] for rel in existing if allowed(rel)):
            print(f"stale allowlist entry: {path}")
            hits.append(Hit(path, 0, "", "the allowlisted file is gone"))
    for hit in hits:
        print(f"{hit.path}:{hit.line}: {hit.why}")
        print(f"    {hit.text}")
    if hits:
        print(
            "A Messenger call on a UI thread has no bound (issue #720): "
            "call `<method>_within(.., Deadline::ticks(n))` for a method the "
            ".midl declares `deadline=never`, or move it to a worker "
            "thread; `Deadline::Never` needs an ALLOWLIST entry with a reason."
        )
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
