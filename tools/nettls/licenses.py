#!/usr/bin/env python3
"""Check that every crate linked into the TLS clients has a GPLv2-compatible licence.

A GPL-2.0-only program (the planned NetSurf port) will link the TLS stack,
so every third-party crate compiled into ``fetch`` and ``tlsfix`` must be
usable under a licence GPL-2.0 can absorb. ring (``Apache-2.0 AND ISC`` plus
OpenSSL-derived code) and aws-lc-rs fail this; Apache-2.0 alone is not
GPLv2-compatible, but ``MIT OR Apache-2.0`` is fine because we take MIT.

How: ``cargo tree -e normal`` for each manifest and the musl target, which
applies cargo's real feature resolution (``cargo metadata`` lists optional
dependencies such as webpki's ``ring`` even when no feature enables them).
Build-time-only crates (proc macros, build scripts) are listed but not
judged, since none of their code ships. Workspace-local crates (no registry
source) are LazyOS's own and reported separately. Each SPDX expression is
evaluated: ``OR`` needs one acceptable side, ``AND`` needs both, and the
pre-SPDX ``/`` separator means ``OR``.

Usage::

    python tools/nettls/licenses.py            # nettls/ and tools/abi/fixtures/tlsfix/
    python tools/nettls/licenses.py --lazyweb  # also xui-app/web/ (LazyWeb, GPL-2.0-only)
    python tools/nettls/licenses.py --lazywebplus  # also xui-app/webplus/ (LazyWeb+'s window, docs/lazywebplus.md)
    python tools/nettls/licenses.py --lazysky  # also xui-app/crates/lazysky-core/
    python tools/nettls/licenses.py --verbose  # also print every crate and its licence

Exit status 1 when any linked crate has no acceptable licence choice.
"""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent
MANIFESTS = [ROOT / "nettls" / "Cargo.toml", ROOT / "tools" / "abi" / "fixtures" / "tlsfix" / "Cargo.toml"]
#: LazyWeb links the TLS stack into a program still declared GPL-2.0-only (the
#: licence NetSurf required; relicensing is open, issue #649), so its whole
#: tree must be GPLv2-compatible too. Opt-in (``--lazyweb``) until the tree
#: passes: `unicode-linebreak` (Apache-2.0, through xui-canvas's
#: cosmic-text) does not yet. Its manifest is a member of the `xui-app`
#: workspace; `cargo tree` reports just this package's graph.
LAZYWEB = ROOT / "xui-app" / "web" / "Cargo.toml"
#: LazyWeb+'s window (docs/lazywebplus.md) is LazyWeb's chrome on WebKit: it links no TLS stack of its
#: own (WebKit's curl does the HTTPS), but it keeps LazyWeb's rule for what is linked into it, so
#: its tree (and `xui-webkit`'s, the engine seam it links) must be GPLv2-compatible too. The program
#: it ends up in also links OpenSSL (Apache-2.0) through WebKit, which `tools/webkit/licenses.py`
#: covers: that is why the crate is declared `GPL-2.0-only OR GPL-3.0-or-later`.
LAZYWEBPLUS = ROOT / "xui-app" / "webplus" / "Cargo.toml"
#: What the LazyWeb+ check lets through that a GPL-2.0-only program could not carry. The program
#: links OpenSSL (Apache-2.0) through WebKit's curl, so it is distributed as GPL-3.0-or-later,
#: which Apache-2.0 code may join; `unicode-linebreak` (Apache-2.0 only, through xui-canvas's
#: cosmic-text, linked into every xui app) is that case. The window crate must offer GPL-3.0-or-later
#: for the waiver to hold (`check` verifies its `license` field).
LAZYWEBPLUS_WAIVERS = {"unicode-linebreak"}
#: LazySky links the same TLS stack (docs/lazysky.md); its headless core is the
#: part that does, so its tree is checked with ``--lazysky``.
LAZYSKY = ROOT / "xui-app" / "crates" / "lazysky-core" / "Cargo.toml"
TARGET = "x86_64-unknown-linux-musl"

#: Licences a GPL-2.0 binary can include (FSF's GPL-compatible list, the
#: permissive subset, plus GPL-2.0 itself).
ACCEPTABLE = {
    "MIT", "MIT-0", "ISC", "0BSD", "BSD-2-Clause", "BSD-3-Clause", "Zlib",
    "Unicode-3.0", "Unicode-DFS-2016", "Unlicense", "CC0-1.0", "BSL-1.0",
    "GPL-2.0", "GPL-2.0-only", "GPL-2.0-or-later", "GPL-2.0+",
    "LGPL-2.1", "LGPL-2.1-only", "LGPL-2.1-or-later",
    # MPL-2.0 section 3.3 lets it combine with GPL code as a Secondary License
    # (Stylo, Blitz's style system).
    "MPL-2.0",
    # LLVM's exception exists precisely to make Apache-2.0 GPLv2-compatible.
    "Apache-2.0 WITH LLVM-exception",
}


def tokenize(expression: str) -> list[str]:
    text = expression.replace("/", " OR ").replace("(", " ( ").replace(")", " ) ")
    raw = text.split()
    tokens: list[str] = []
    i = 0
    while i < len(raw):
        # Keep `X WITH exception` as one licence.
        if i + 2 < len(raw) and raw[i + 1] == "WITH":
            tokens.append(f"{raw[i]} WITH {raw[i + 2]}")
            i += 3
        else:
            tokens.append(raw[i])
            i += 1
    return tokens


def acceptable(expression: str, allowed: set[str] | None = None) -> bool:
    """Whether the SPDX `expression` allows a GPLv2-compatible choice (or, with
    `allowed`, one from that set: ``tools/webkit/licenses.py`` brings its own)."""
    allowed = ACCEPTABLE if allowed is None else allowed
    tokens = tokenize(expression)
    pos = 0

    def parse_or() -> bool:
        nonlocal pos
        value = parse_and()
        while pos < len(tokens) and tokens[pos] == "OR":
            pos += 1
            right = parse_and()
            value = value or right
        return value

    def parse_and() -> bool:
        nonlocal pos
        value = parse_atom()
        while pos < len(tokens) and tokens[pos] == "AND":
            pos += 1
            right = parse_atom()
            value = value and right
        return value

    def parse_atom() -> bool:
        nonlocal pos
        if pos >= len(tokens):
            raise ValueError(f"truncated licence expression: {expression!r}")
        token = tokens[pos]
        pos += 1
        if token == "(":
            value = parse_or()
            if pos >= len(tokens) or tokens[pos] != ")":
                raise ValueError(f"unbalanced licence expression: {expression!r}")
            pos += 1
            return value
        return token in allowed

    value = parse_or()
    if pos != len(tokens):
        raise ValueError(f"cannot parse licence expression: {expression!r}")
    return value


def tree(manifest: Path, edges: str) -> dict[str, tuple[str, bool]]:
    """`{"name version": (licence, local)}` for every package `cargo tree`
    reaches over `edges` for the musl target (cargo's real feature
    resolution, so optional dependencies nobody enables are not counted)."""
    done = subprocess.run(
        ["cargo", "tree", "--locked", "--manifest-path", str(manifest), "-e", edges,
         "--target", TARGET, "--prefix", "none", "--format", "{p}|{l}"],
        capture_output=True, text=True, check=True,
    )
    found: dict[str, tuple[str, bool]] = {}
    for line in done.stdout.splitlines():
        if "|" not in line:
            continue
        package, licence = line.split("|", 1)
        licence = licence.replace("(*)", "").strip()
        words = package.replace("(*)", "").split()
        # A local (path) package prints its directory after the version.
        if "(proc-macro)" in words:
            continue  # runs in the compiler; none of its code is linked
        # A path package prints "(/dir)" or "(C:\\dir)"; a git one prints
        # "(https://...)" and is third-party, so its licence is checked.
        source = words[2] if len(words) > 2 else ""
        local = source.startswith("(") and "://" not in source
        found[" ".join(words[:2])] = (licence, local)
    return found


def linked_packages(manifest: Path) -> tuple[dict, dict, list[str]]:
    """(third-party linked, local linked, build-only names) of `manifest`."""
    linked = tree(manifest, "normal")
    everything = tree(manifest, "normal,build")
    third = {k: v[0] for k, v in linked.items() if not v[1]}
    local = {k: v[0] for k, v in linked.items() if v[1]}
    build_only = sorted(set(everything) - set(linked))
    return third, local, build_only


def check(manifest: Path, verbose: bool, waivers: frozenset[str] = frozenset()) -> list[str]:
    third, local, build_only = linked_packages(manifest)
    problems: list[str] = []
    label = manifest.parent.relative_to(ROOT)
    if waivers and "GPL-3.0-or-later" not in manifest.read_text(encoding="utf-8"):
        problems.append(f"{label}: waivers {sorted(waivers)} need the crate to offer GPL-3.0-or-later (its license field)")
    for package, expression in sorted(third.items()):
        try:
            ok = bool(expression) and acceptable(expression)
        except ValueError as error:
            ok, expression = False, f"{expression} ({error})"
        line = f"{package}: {expression or 'NO LICENCE FIELD'}"
        if not ok and package.split()[0] in waivers:
            print(f"  waived {line} (the program is GPL-3.0-or-later, see LAZYWEBPLUS_WAIVERS)")
            ok = True
        if not ok:
            problems.append(f"{label}: {line}")
        if verbose:
            print(f"  {'ok ' if ok else 'BAD'} {line}")
    print(f"{label}: {len(third)} linked third-party crates, {len(problems)} without a GPLv2-compatible licence")
    print(f"{label}: local crates (LazyOS's own): "
          + ", ".join(f"{name} ({licence or '?'})" for name, licence in sorted(local.items())))
    if verbose:
        print(f"{label}: build-time only (not linked): " + ", ".join(build_only))
    return problems


def self_test() -> None:
    assert acceptable("MIT OR Apache-2.0")
    assert acceptable("Apache-2.0 OR ISC OR MIT")
    assert acceptable("MIT/Apache-2.0")
    assert acceptable("(MIT OR Apache-2.0) AND Unicode-3.0")
    assert acceptable("Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT")
    assert acceptable("BSD-3-Clause")
    assert acceptable("MPL-2.0")
    assert not acceptable("Apache-2.0")
    assert not acceptable("Apache-2.0 AND ISC")
    assert not acceptable("MIT AND Apache-2.0")
    assert not acceptable("GPL-3.0-or-later")
    assert not acceptable("OpenSSL")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--verbose", action="store_true", help="print every crate and its licence")
    parser.add_argument("--lazyweb", action="store_true", help="also check LazyWeb (xui-app/web)")
    parser.add_argument("--lazywebplus", action="store_true", help="also check LazyWeb+'s window (xui-app/webplus)")
    parser.add_argument("--lazysky", action="store_true",
                        help="also check LazySky's core (xui-app/crates/lazysky-core)")
    args = parser.parse_args()
    self_test()
    problems: list[str] = []
    for manifest in (MANIFESTS + ([LAZYWEB] if args.lazyweb else []) + ([LAZYWEBPLUS] if args.lazywebplus else [])
                     + ([LAZYSKY] if args.lazysky else [])):
        problems += check(manifest, args.verbose, frozenset(LAZYWEBPLUS_WAIVERS) if manifest == LAZYWEBPLUS else frozenset())
    if problems:
        print("licences not GPLv2-compatible:")
        for line in problems:
            print(f"  {line}")
        return 1
    print("all linked third-party crates have a GPLv2-compatible licence choice")
    return 0


if __name__ == "__main__":
    sys.exit(main())
