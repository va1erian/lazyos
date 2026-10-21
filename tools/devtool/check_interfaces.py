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
  * a `DENY` name is neither in idl/manifest.json nor in `KERNEL_SCOPES`.

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

ADVICE = (
    "decide whether a developer tool may call it: add it to `DENY` in "
    "libs/devtool/src/lib.rs (and the explain table) if it carries secrets or "
    "authority, otherwise add it to tools/devtool/reviewed_interfaces.txt"
)


def manifest_interfaces(text: str) -> list[str]:
    """The interface names idl/manifest.json declares, in file order."""
    return [entry["interface"] for entry in json.loads(text)]


def _const_body(source: str, name: str) -> str:
    """The text between `pub const NAME: ... = ` and the `];` that ends its
    array (the constants are arrays of literals, so no `];` occurs inside)."""
    match = re.search(rf"pub const {name}\b[^=]*=\s*(.*?)\];", source, re.S)
    return match.group(1) if match else ""


def deny_names(source: str) -> list[str]:
    """The `interface: "..."` of every entry of `DENY`."""
    return re.findall(r'interface:\s*"([^"]+)"', _const_body(source, "DENY"))


def kernel_scopes(source: str) -> list[str]:
    """The names in `KERNEL_SCOPES`."""
    return re.findall(r'"([^"]+)"', _const_body(source, "KERNEL_SCOPES"))


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
) -> list[str]:
    """Every problem, one sentence each; empty when all is classified."""
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
    if len(listed) != len(reviewed):
        repeated = sorted({n for n in reviewed if reviewed.count(n) > 1})
        problems.append(f"the reviewed list repeats: {', '.join(repeated)}")
    if reviewed != sorted(reviewed):
        problems.append("the reviewed list is not sorted (one name per line, sorted)")
    return problems


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--manifest", type=Path, default=MANIFEST)
    parser.add_argument("--lib", type=Path, default=LIB)
    parser.add_argument("--reviewed", type=Path, default=REVIEWED)
    args = parser.parse_args(argv)
    interfaces = manifest_interfaces(args.manifest.read_text(encoding="utf-8"))
    source = args.lib.read_text(encoding="utf-8")
    reviewed = reviewed_names(args.reviewed.read_text(encoding="utf-8"))
    deny = deny_names(source)
    problems = check(interfaces, deny, kernel_scopes(source), reviewed)
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
