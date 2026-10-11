#!/usr/bin/env python3
"""Check the Wi-Fi stack's licences: its crates' dependencies and its register.

The Wi-Fi crates end up in `wifid` and the station stack, GPL-3.0-or-later
programs, and `libs/rtw88` is derived from Linux's rtw88 taken under
BSD-3-Clause (docs/wifi-rtw88-plan.md section 2, THIRD_PARTY.md). This
checks, in the spirit of `tools/nettls/licenses.py`:

1. every third-party crate linked into a Wi-Fi crate (``cargo tree -e
   normal`` for the OS target, cargo's real feature resolution) has a licence
   choice GPL-3.0-or-later can absorb (proc macros and build scripts are
   build-time only, so not judged);
2. every Wi-Fi crate of LazyOS's own declares a licence, and ``libs/rtw88``
   declares BSD-3-Clause (its upstream's notice must travel with it);
3. ``THIRD_PARTY.md`` registers the derived code at the commit
   ``rtw88::UPSTREAM`` names, with the BSD-3-Clause notice text, and every
   firmware file of ``tools/firmware/manifest.txt`` with its digest and
   origin commit.

    python tools/wifi/licenses.py            # check
    python tools/wifi/licenses.py --verbose  # also print every crate

Exit status 1 when anything fails. Add a crate to ``CRATES`` when the stack
grows one (``libs/wifimac``, the programs' crates).
"""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent
sys.path.insert(0, str(ROOT / "tools" / "firmware"))
import fetch  # noqa: E402  (the firmware manifest's parser)

#: The Wi-Fi crates (a missing one is skipped: the stack lands in stages).
CRATES = ["libs/rtw88", "libs/fwload", "libs/ieee80211", "libs/eapol", "libs/wlansta", "libs/wifimac"]
TARGET = "x86_64-unknown-none"
THIRD_PARTY = ROOT / "THIRD_PARTY.md"
RTW88_LIB = ROOT / "libs" / "rtw88" / "src" / "lib.rs"

#: Licences a GPL-3.0-or-later program can include.
ACCEPTABLE = {
    "MIT", "MIT-0", "ISC", "0BSD", "BSD-2-Clause", "BSD-3-Clause", "BSD-3-Clause-Clear",
    "Zlib", "Unicode-3.0", "Unicode-DFS-2016", "Unlicense", "CC0-1.0", "BSL-1.0",
    "Apache-2.0", "Apache-2.0 WITH LLVM-exception", "MPL-2.0",
    "GPL-2.0-or-later", "GPL-2.0+", "GPL-3.0", "GPL-3.0-only", "GPL-3.0-or-later",
    "LGPL-2.1-or-later", "LGPL-3.0-or-later",
}

#: Sentences of the BSD-3-Clause notice that must appear in THIRD_PARTY.md.
BSD3_CLAUSES = (
    "Redistributions of source code must retain the above copyright notice",
    "Redistributions in binary form must reproduce the above copyright notice",
    "Neither the name of the copyright holder nor the names of its",
    'THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS"',
)


def accepted(expression: str) -> bool:
    """Whether the SPDX `expression` allows a choice in `ACCEPTABLE` (`OR`
    needs one side, `AND` both; the pre-SPDX `/` means `OR`)."""
    text = expression.replace("/", " OR ").replace("(", " ( ").replace(")", " ) ")
    raw, tokens = text.split(), []
    while raw:
        if len(raw) > 2 and raw[1] == "WITH":
            tokens.append(" ".join(raw[:3]))
            raw = raw[3:]
        else:
            tokens.append(raw.pop(0))
    pos = 0

    def any_of() -> bool:
        nonlocal pos
        value = all_of()
        while pos < len(tokens) and tokens[pos] == "OR":
            pos += 1
            value = all_of() or value
        return value

    def all_of() -> bool:
        nonlocal pos
        value = atom()
        while pos < len(tokens) and tokens[pos] == "AND":
            pos += 1
            value = atom() and value
        return value

    def atom() -> bool:
        nonlocal pos
        if pos >= len(tokens):
            raise ValueError(f"truncated licence expression: {expression!r}")
        token, pos = tokens[pos], pos + 1
        if token != "(":
            return token in ACCEPTABLE
        value = any_of()
        if pos >= len(tokens) or tokens[pos] != ")":
            raise ValueError(f"unbalanced licence expression: {expression!r}")
        pos += 1
        return value

    value = any_of()
    if pos != len(tokens):
        raise ValueError(f"cannot parse licence expression: {expression!r}")
    return value


def tree(manifest: Path) -> dict[str, tuple[str, bool]]:
    """`{"name version": (licence, local)}` linked into `manifest`'s crate."""
    fmt = "{p}" + "|" + "{l}"
    done = subprocess.run(
        ["cargo", "tree", "--locked", "--manifest-path", str(manifest), "-e", "normal",
         "--target", TARGET, "--prefix", "none", "--format", fmt],
        capture_output=True, text=True, check=True, cwd=ROOT,
    )
    found: dict[str, tuple[str, bool]] = {}
    for line in done.stdout.splitlines():
        if "|" not in line:
            continue
        package, licence = line.split("|", 1)
        words = package.replace("(*)", "").split()
        if "(proc-macro)" in words:
            continue
        source = words[2] if len(words) > 2 else ""
        local = source.startswith("(") and "://" not in source
        found[" ".join(words[:2])] = (licence.replace("(*)", "").strip(), local)
    return found


def check_crates(verbose: bool) -> list[str]:
    problems: list[str] = []
    for crate in CRATES:
        manifest = ROOT / crate / "Cargo.toml"
        if not manifest.is_file():
            continue
        for package, (licence, local) in sorted(tree(manifest).items()):
            if local:
                ok = bool(licence)
                if package.startswith("rtw88 ") and "BSD-3-Clause" not in licence:
                    ok = False
            else:
                try:
                    ok = bool(licence) and accepted(licence)
                except ValueError:
                    ok = False
            if not ok:
                problems.append(f"{crate}: {package}: {licence or 'NO LICENCE FIELD'}")
            if verbose:
                print(f"  {'ok ' if ok else 'BAD'} {crate}: {package}: {licence}")
    return problems


def upstream_commit() -> str:
    match = re.search(r'pub const UPSTREAM: &str = "([0-9a-f]{40})"',
                      RTW88_LIB.read_text(encoding="utf-8"))
    if not match:
        raise ValueError(f"{RTW88_LIB}: no UPSTREAM commit")
    return match.group(1)


def check_register(text: str, commit: str, blobs: list) -> list[str]:
    """What THIRD_PARTY.md must say about the Wi-Fi stack's sources."""
    problems: list[str] = []
    rows = [line for line in text.splitlines() if line.startswith("|")]
    rtw88 = [row for row in rows if "libs/rtw88" in row]
    if not any(commit in row and "BSD-3-Clause" in row for row in rtw88):
        problems.append(f"THIRD_PARTY.md: no `libs/rtw88` row with commit {commit} and BSD-3-Clause")
    if "Realtek" not in text:
        problems.append("THIRD_PARTY.md: Realtek's copyright is not carried")
    for clause in BSD3_CLAUSES:
        if clause not in text:
            problems.append(f"THIRD_PARTY.md: the BSD-3-Clause notice lacks: {clause!r}")
    for blob in blobs:
        commit_in_url = re.search(r"/([0-9a-f]{40})/", blob.url)
        if not any(blob.name in row and blob.sha256 in row
                   and (commit_in_url is None or commit_in_url.group(1) in row) for row in rows):
            problems.append(f"THIRD_PARTY.md: no row for {blob.name} with its SHA-256 and commit")
    return problems


def self_test() -> None:
    assert accepted("MIT OR Apache-2.0")
    assert accepted("Apache-2.0")
    assert accepted("(MIT OR Apache-2.0) AND Unicode-3.0")
    assert accepted("GPL-3.0-or-later AND BSD-3-Clause")
    assert not accepted("GPL-2.0-only")
    assert not accepted("GPL-2.0")
    assert not accepted("MIT AND GPL-2.0-only")
    assert not accepted("LicenseRef-proprietary")
    good = ("| `libs/rtw88` | Linux d6d3 0123 | GPL-2.0 OR BSD-3-Clause, taken under BSD-3-Clause |\n"
            "Realtek\n" + "\n".join(BSD3_CLAUSES) + "\n| fw/a.bin | x/0123456789012345678901234567890123456789/ | ab |")
    blob = fetch.Blob("fw/a.bin", "wifi", "ab", 1, "fw/a.bin",
                      "https://h/0123456789012345678901234567890123456789/a")
    assert check_register(good, "0123", [blob]) == []
    assert check_register(good.replace("GPL-2.0 OR BSD-3-Clause, taken under BSD-3-Clause",
                                       "GPL-2.0"), "0123", [blob])
    assert check_register(good, "4567", [blob])
    assert check_register(good.replace("| ab |", "| cd |"), "0123", [blob])


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--verbose", action="store_true", help="print every crate and its licence")
    args = parser.parse_args()
    self_test()
    problems = check_crates(args.verbose)
    blobs = fetch.load_manifest()
    problems += check_register(THIRD_PARTY.read_text(encoding="utf-8"), upstream_commit(), blobs)
    if problems:
        print("Wi-Fi licence problems:")
        for line in problems:
            print(f"  {line}")
        return 1
    print(f"Wi-Fi crates ({', '.join(c for c in CRATES if (ROOT / c).is_dir())}): every linked "
          f"crate GPLv3-compatible; THIRD_PARTY.md registers rtw88 at {upstream_commit()[:12]} "
          f"and {len(blobs)} firmware file(s)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
