#!/usr/bin/env python3
"""Every interface in idl/ is classified for developer tools.

A developer tool (docs/devtool-packages-plan.md) is default allow minus a
short deny list, `DENY` in libs/devtool/src/lib.rs. That is only safe while a
new interface cannot become callable by tools without somebody deciding it, so
this check compares the interfaces in idl/manifest.json with that list and
with tools/devtool/reviewed_interfaces.txt (one name per line, sorted; blank
lines and `#` comments are ignored) and fails when

  * an interface is in neither (decide: add it to `devtool::DENY` if it
    carries secrets or authority a tool must not hold, else to the reviewed
    list);
  * a name is on both lists, or the reviewed list names an interface idl/ no
    longer declares, or it is unsorted or repeats a name;
  * a `DENY` name is neither in idl/manifest.json nor in `KERNEL_SCOPES`;
  * the kernel tests' stand-in for `DENY` (`DENIED` in
    kernel/src/tests/label_suite/tool.rs; the kernel cannot depend on
    `devtool`) names other interfaces than `DENY` does, so the kernel tests
    would judge a tool by a list that is not the shipped one;
  * the stand-in's `when` lists (`UNLESS_NETWORK`, `UNLESS_DEVELOP`,
    `UNLESS_SHELL`) differ from the `DENY` entries of that `when` (which
    declaration lifts which entry), or a `DENY` entry has a `when` the check
    does not know. (pkgstore derives its own network list from `DENY`.)

    python tools/devtool/check_interfaces.py
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
MANIFEST = ROOT / "idl" / "manifest.json"
LIB = ROOT / "libs" / "devtool" / "src" / "lib.rs"
REVIEWED = ROOT / "tools" / "devtool" / "reviewed_interfaces.txt"
STAND_IN = ROOT / "kernel" / "src" / "tests" / "label_suite" / "tool.rs"

ADVICE = (
    "decide whether a developer tool may call it: add it to `DENY` in "
    "libs/devtool/src/lib.rs (and the kernel tests' stand-in list in "
    "kernel/src/tests/label_suite/tool.rs, which this check compares; the "
    "consent screen's rows are derived from DENY) if it carries secrets or "
    "authority, otherwise add it to tools/devtool/reviewed_interfaces.txt"
)


def manifest_interfaces(text: str) -> list[str]:
    """The interface names idl/manifest.json declares, in file order."""
    return [entry["interface"] for entry in json.loads(text)]


def _const_body(source: str, name: str) -> str:
    """The text between `pub const NAME: ... = ` and the `];` that ends its
    array (the constants are arrays of literals, so no `];` occurs inside)."""
    match = re.search(
        rf"pub(?:\([a-z]+\))? const {name}\b[^=]*=\s*(.*?)\];", source, re.S
    )
    return match.group(1) if match else ""


def deny_names(source: str) -> list[str]:
    """The `interface: "..."` of every entry of `DENY`."""
    return re.findall(r'interface:\s*"([^"]+)"', _const_body(source, "DENY"))


# `DenyWhen` variants that lift an entry, and the stand-in constant for each.
# `Always` entries are in no list.
WHEN_CONSTS = {
    "UnlessNetwork": "UNLESS_NETWORK",
    "UnlessDevelop": "UNLESS_DEVELOP",
    "UnlessShell": "UNLESS_SHELL",
}


def deny_whens(source: str) -> list[tuple[str, str]]:
    """The `(interface, DenyWhen variant)` of every entry of `DENY`."""
    return re.findall(
        r'interface:\s*"([^"]+)",(?:\s|//[^\n]*)*when:\s*DenyWhen::(\w+)',
        _const_body(source, "DENY"),
    )


def stand_in_whens(source: str) -> dict[str, list[str]]:
    """The names in each `when` list of the kernel tests' stand-in, keyed by
    the `DenyWhen` variant they stand for."""
    return {
        when: re.findall(r'"([^"]+)"', _const_body(source, const))
        for when, const in WHEN_CONSTS.items()
    }


def kernel_scopes(source: str) -> list[str]:
    """The names in `KERNEL_SCOPES`."""
    return re.findall(r'"([^"]+)"', _const_body(source, "KERNEL_SCOPES"))


def stand_in_names(source: str) -> list[str]:
    """The names in the kernel tests' `DENIED` stand-in."""
    return re.findall(r'"([^"]+)"', _const_body(source, "DENIED"))


def reviewed_names(text: str) -> list[str]:
    """The names in the reviewed list, in file order."""
    names = []
    for line in text.splitlines():
        line = line.strip()
        if line and not line.startswith("#"):
            names.append(line)
    return names


def check(
    interfaces: list[str],
    deny: list[str],
    scopes: list[str],
    reviewed: list[str],
    stand_in: list[str] | None = None,
    deny_when: list[tuple[str, str]] | None = None,
    stand_in_when: dict[str, list[str]] | None = None,
) -> list[str]:
    """Every problem, one sentence each; empty when all is classified.
    `stand_in` is the kernel tests' copy of the deny list (None: not checked);
    `deny_when` the `(name, DenyWhen variant)` of each `DENY` entry and
    `stand_in_when` the stand-in's lists per variant (both None: not checked)."""
    problems: list[str] = []
    if not deny:
        problems.append("found no DENY entries in libs/devtool/src/lib.rs")
    declared = set(interfaces)
    denied = set(deny)
    listed = set(reviewed)
    for name in sorted(declared - denied - listed):
        problems.append(f"{name} is new: {ADVICE}")
    for name in sorted(denied & listed):
        problems.append(f"{name} is in DENY and in the reviewed list: keep it in one")
    for name in sorted(listed - declared):
        problems.append(
            f"{name} is in the reviewed list but idl/ does not declare it: remove the line"
        )
    for name in sorted(denied - declared - set(scopes)):
        problems.append(
            f"{name} is in DENY but is neither declared in idl/ nor in KERNEL_SCOPES"
        )
    if stand_in is not None:
        if not stand_in:
            problems.append(
                "found no DENIED entries in kernel/src/tests/label_suite/tool.rs"
            )
        for name in sorted(denied - set(stand_in)):
            problems.append(
                f"{name} is in DENY but not in the kernel tests' DENIED "
                "(kernel/src/tests/label_suite/tool.rs): add it, so the kernel "
                "tests judge a tool by the shipped list"
            )
        for name in sorted(set(stand_in) - denied):
            problems.append(
                f"{name} is in the kernel tests' DENIED "
                "(kernel/src/tests/label_suite/tool.rs) but not in DENY: remove it "
                "or add it to DENY"
            )
        if len(set(stand_in)) != len(stand_in):
            repeated = sorted({n for n in stand_in if stand_in.count(n) > 1})
            problems.append(f"the kernel tests' DENIED repeats: {', '.join(repeated)}")
    if deny_when is not None and stand_in_when is not None:
        problems.extend(check_whens(deny_when, stand_in_when))
    if len(listed) != len(reviewed):
        repeated = sorted({n for n in reviewed if reviewed.count(n) > 1})
        problems.append(f"the reviewed list repeats: {', '.join(repeated)}")
    if reviewed != sorted(reviewed):
        problems.append("the reviewed list is not sorted (one name per line, sorted)")
    return problems


def check_whens(
    deny_when: list[tuple[str, str]], stand_in_when: dict[str, list[str]]
) -> list[str]:
    """Problems with the `when` column: each variant that lifts an entry has
    the same names in `DENY` and in the kernel tests' stand-in."""
    problems: list[str] = []
    for name, when in deny_when:
        if when != "Always" and when not in WHEN_CONSTS:
            problems.append(
                f"{name} has `DenyWhen::{when}`, which check_interfaces.py does not "
                "know: add it to WHEN_CONSTS and the kernel tests' stand-in"
            )
    for when, const in WHEN_CONSTS.items():
        shipped = {name for name, w in deny_when if w == when}
        copy = stand_in_when.get(when, [])
        for name in sorted(shipped - set(copy)):
            problems.append(
                f"{name} is `DenyWhen::{when}` in DENY but not in {const} "
                "(kernel/src/tests/label_suite/tool.rs): the kernel tests would "
                "treat it as lifted by another declaration"
            )
        for name in sorted(set(copy) - shipped):
            problems.append(
                f"{name} is in {const} (kernel/src/tests/label_suite/tool.rs) but "
                f"is not `DenyWhen::{when}` in DENY: fix the list"
            )
        if len(set(copy)) != len(copy):
            problems.append(f"{const} repeats a name")
    return problems


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--manifest", type=Path, default=MANIFEST)
    parser.add_argument("--lib", type=Path, default=LIB)
    parser.add_argument("--reviewed", type=Path, default=REVIEWED)
    parser.add_argument("--stand-in", type=Path, default=STAND_IN)
    args = parser.parse_args(argv)
    interfaces = manifest_interfaces(args.manifest.read_text(encoding="utf-8"))
    source = args.lib.read_text(encoding="utf-8")
    reviewed = reviewed_names(args.reviewed.read_text(encoding="utf-8"))
    deny = deny_names(source)
    stand_in_source = args.stand_in.read_text(encoding="utf-8")
    problems = check(
        interfaces,
        deny,
        kernel_scopes(source),
        reviewed,
        stand_in_names(stand_in_source),
        deny_whens(source),
        stand_in_whens(stand_in_source),
    )
    for problem in problems:
        print(f"check_interfaces: {problem}", file=sys.stderr)
    if problems:
        return 1
    print(
        f"check_interfaces: PASS {len(interfaces)} interfaces "
        f"({len(deny)} denied, {len(reviewed)} reviewed)"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
