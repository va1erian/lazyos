#!/usr/bin/env python3
"""Check the licences of the WebKit dependency bundle (``tools/webkit``).

The libraries end up in ``depsproof`` and, statically, in the WebKit port's programs
(LGPL-2.1 WebKit, shipped in a GPL-3.0-or-later image), in the spirit of
``tools/nettls/licenses.py`` and ``tools/wifi/licenses.py``:

1. every library of ``wkpins.DEPS`` and ``BUNDLED`` has an SPDX expression
   with a choice an LGPL-2.1 binary can absorb (``LGPL21_OK``);
2. the one exception, **Apache-2.0** (OpenSSL), is GPLv2-incompatible: it is
   accepted only for a library named in ``wkpins.NEVER_IN_LAZYWEB``, that
   library's entry says why, the README and ``THIRD_PARTY.md`` say it must never
   be linked into LazyWeb (GPL-2.0-only) or the ``nettls`` stack, and LazyWeb's
   sources do not reference this bundle or depend on OpenSSL;
3. ``THIRD_PARTY.md`` has a row for every library (name, version, full
   SHA-256, licence expression), for WebKit itself and for the embedded font; ``--rows`` prints
   the rows to paste;
4. when ``target/webkit/deps.json`` exists (a build ran), its licences are the
   pins' and none is missing from the allowlist.

    python tools/webkit/licenses.py            # check, print the table
    python tools/webkit/licenses.py --rows     # the THIRD_PARTY.md rows

Exit status 1 when anything fails.
"""

from __future__ import annotations

import argparse
import importlib.util
import json
import re
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import wkpins  # noqa: E402  (also puts tools/jsc on sys.path)

ROOT = wkpins.ROOT
THIRD_PARTY = ROOT / "THIRD_PARTY.md"
README = HERE / "README.md"
MANIFEST = wkpins.MANIFEST

#: Licences an LGPL-2.1 binary (inside a GPL-3.0-or-later image) can include.
#: Apache-2.0 is not here: see ``APACHE``.
LGPL21_OK = {
    "MIT", "MIT-0", "ISC", "0BSD", "BSD-2-Clause", "BSD-3-Clause", "Zlib", "libpng-2.0", "IJG",
    "FTL", "HPND-sell-variant", "blessing", "curl", "Unicode-3.0", "Unicode-DFS-2016",
    "CC0-1.0", "BSL-1.0", "OFL-1.1",
    # No GPL here: a GPL-only library would make the whole LGPL-2.1 binary GPL.
    # FreeType is `FTL OR GPL-2.0-or-later` and passes through FTL.
    "LGPL-2.1-only", "LGPL-2.1-or-later",
    # MPL-2.0 section 3.3 lets it combine with (L)GPL code as a Secondary License.
    "MPL-2.0",
}
#: GPLv2-incompatible, allowed only for `wkpins.NEVER_IN_LAZYWEB`.
APACHE = "Apache-2.0"
#: What LazyWeb (GPL-2.0-only) and the TLS stack may include: nettls's list plus
#: the licences below, which are GPLv2-compatible permissive terms.
EXTRA_GPL2_OK = {"libpng-2.0", "IJG", "HPND-sell-variant", "blessing", "curl"}
#: Where LazyWeb lives; none of it may use this bundle.
LAZYWEB_TREES = ["xui-app/web", "xui-app/packages/lazyweb", "tools/web"]
BUNDLE_WORDS = ("tools/webkit", "target/webkit", "LAZYOS_WEBKIT_DEPS", "webkit-deps", "depsproof")


def nettls_licenses():
    """tools/nettls/licenses.py by path (its SPDX evaluator and GPLv2 list)."""
    path = ROOT / "tools" / "nettls" / "licenses.py"
    spec = importlib.util.spec_from_file_location("nettls_licenses", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def libraries() -> list[wkpins.Dep]:
    """The bundle's libraries and WebKit itself (the licences of everything that is shipped)."""
    return [*wkpins.DEPS, *wkpins.BUNDLED, wkpins.WEBKIT]


def verdict(dep: wkpins.Dep, nettls) -> tuple[bool, bool, str]:
    """(ok for the LGPL-2.1 binary, ok for GPL-2.0-only code, problem)."""
    exempt = dep.name in wkpins.NEVER_IN_LAZYWEB
    allowed = LGPL21_OK | ({APACHE} if exempt else set())
    try:
        lgpl = nettls.acceptable(dep.license, allowed)
        gpl2 = nettls.acceptable(dep.license, nettls.ACCEPTABLE | EXTRA_GPL2_OK)
    except ValueError as error:
        return False, False, f"{dep.name}: {error}"
    problem = ""
    if not lgpl:
        problem = f"{dep.name}: licence {dep.license!r} is not on the LGPL-2.1 allowlist"
    elif not gpl2 and not exempt:
        problem = (f"{dep.name}: licence {dep.license!r} is GPLv2-incompatible but the library is "
                   "not in wkpins.NEVER_IN_LAZYWEB")
    elif exempt and gpl2:
        problem = f"{dep.name} is in NEVER_IN_LAZYWEB but its licence {dep.license!r} is GPLv2-compatible"
    elif exempt and "LazyWeb" not in dep.notes:
        problem = f"{dep.name}: a GPLv2-incompatible library must say in its notes that LazyWeb never links it"
    return lgpl, gpl2, problem


def table(nettls) -> list[str]:
    lines = [f"{'library':<24}{'version':<12}{'licence':<40}{'GPLv2 code':<11}notes"]
    for dep in libraries():
        _, gpl2, _ = verdict(dep, nettls)
        lines.append(f"{dep.name:<24}{dep.source.version:<12}{dep.license:<40}{'yes' if gpl2 else 'NO':<11}"
                     f"{dep.notes}")
    for data in wkpins.DATA_FILES:
        lines.append(f"{data.name:<24}{data.version:<12}{data.license:<40}{'(data)':<11}{data.path}")
    return lines


def rows() -> list[str]:
    """The ``THIRD_PARTY.md`` rows: name and version, origin, licence, files."""
    out = []
    for dep in libraries():
        extra = f"; {dep.notes}" if dep.notes else ""
        out.append(f"| {dep.name} {dep.source.version} | `{dep.source.url}`, SHA-256 `{dep.source.sha256}` "
                   f"| {dep.license}{extra} | {dep.linked or wkpins.DEPSPROOF_LINK} |")
    for data in wkpins.DATA_FILES:
        out.append(f"| {data.name} {data.version} | `{data.path}` (`assets/manifest.txt`) | {data.license} "
                   f"| compiled into `/system/bin/depsproof` as its test font |")
    return out


def check_register(text: str) -> list[str]:
    """Problems with the ``THIRD_PARTY.md`` rows of the bundle."""
    problems = []
    lines = [line for line in text.splitlines() if line.startswith("|")]
    for dep in libraries():
        head = f"| {dep.name} {dep.source.version} |"
        row = next((line for line in lines if line.startswith(head)), None)
        if row is None:
            problems.append(f"THIRD_PARTY.md has no row starting {head!r}")
        elif dep.source.sha256 not in row or not re.search(rf"\| {re.escape(dep.license)}(;| \|)", row):
            problems.append(f"THIRD_PARTY.md row of {dep.name} lacks its SHA-256 or licence {dep.license!r}")
    for data in wkpins.DATA_FILES:
        if not any(line.startswith(f"| {data.name} {data.version} |")
                   and re.search(rf"\| {re.escape(data.license)} \|", line) for line in lines):
            problems.append(f"THIRD_PARTY.md has no row for {data.name} {data.version} ({data.license})")
    return problems


def check_notices(third_party: str, readme: str) -> list[str]:
    """OpenSSL's must-not-link-into-LazyWeb rule is written down where readers look."""
    problems = []
    for name in wkpins.NEVER_IN_LAZYWEB:
        label = wkpins.BY_NAME[name].name if name in wkpins.BY_NAME else name
        pattern = rf"{label}[^\n]*\n?[^\n]*never[^\n]*LazyWeb|never[^\n]*LazyWeb[^\n]*{label}"
        for what, text in (("README.md", readme), ("THIRD_PARTY.md", third_party)):
            if not re.search(pattern, text, re.I | re.S):
                problems.append(f"{what} must say that {label} is never linked into LazyWeb")
    return problems


def check_lazyweb_apart() -> list[str]:
    """LazyWeb's sources must not use the bundle or depend on OpenSSL."""
    problems = []
    for tree in LAZYWEB_TREES:
        base = ROOT / tree
        if not base.is_dir():
            continue
        for path in sorted(base.rglob("*")):
            if not path.is_file() or path.suffix not in {".rs", ".toml", ".py", ".json", ".md", ".txt", ".lock"}:
                continue
            if "target" in path.relative_to(base).parts:
                continue
            text = path.read_text(encoding="utf-8", errors="replace")
            for word in BUNDLE_WORDS:
                if word in text:
                    problems.append(f"{path.relative_to(ROOT).as_posix()} mentions {word!r}: "
                                    "LazyWeb (GPL-2.0-only) must not use the WebKit bundle")
            if path.name == "Cargo.toml" and re.search(r"^\s*(openssl|openssl-sys|native-tls)\b", text, re.M):
                problems.append(f"{path.relative_to(ROOT).as_posix()} depends on OpenSSL: "
                                "Apache-2.0 is GPLv2-incompatible")
    return problems


#: LazyWeb+'s window crates: LazyWeb's chrome (GPL-2.0-only) and the seam it links, in a program that
#: also links OpenSSL (docs/lazywebplus.md, THIRD_PARTY.md).
WEBPLUS_CRATES = ["xui-app/webplus/Cargo.toml", "xui-app/crates/webkit/Cargo.toml"]


def check_webplus_licence() -> list[str]:
    """The program LazyWeb+ ends up in links OpenSSL (Apache-2.0), which a GPL-2.0-only program cannot
    carry, so its window crates must be offered under GPL-3.0-or-later as well as GPL-2.0-only (the
    rule LazyWeb keeps for what is linked into the window), and never reference the bundle's
    OpenSSL themselves."""
    problems = []
    for name in WEBPLUS_CRATES:
        path = ROOT / name
        if not path.is_file():
            continue
        text = path.read_text(encoding="utf-8")
        found = re.search(r'^license\s*=\s*"([^"]+)"', text, re.M)
        expression = found.group(1) if found else ""
        # Exactly the choice of the two: an `AND`, or a third licence, is a different promise.
        choices = sorted(part.strip().strip("()") for part in re.split(r"\s+OR\s+", expression))
        if choices != ["GPL-2.0-only", "GPL-3.0-or-later"]:
            problems.append(f"{name}: license {expression!r} must be exactly 'GPL-2.0-only OR GPL-3.0-or-later' "
                            "(the program links OpenSSL, Apache-2.0: docs/lazywebplus.md)")
        if re.search(r"^\s*\[(?:\w+-)?dependencies\.(openssl|openssl-sys|native-tls)\]", text, re.M) or re.search(
                r"^\s*(openssl|openssl-sys|native-tls)\b", text, re.M):
            problems.append(f"{name} depends on OpenSSL: Apache-2.0 is GPLv2-incompatible")
    return problems


def check_manifest(manifest: dict) -> list[str]:
    """The licences a build recorded are the pins' (a stale build is named)."""
    problems = []
    pins = {dep.name: dep for dep in libraries()}
    for record in manifest.get("libraries", []):
        dep = pins.get(record.get("name"))
        if dep is None:
            problems.append(f"deps.json has {record.get('name')!r}, which wkpins does not pin")
        elif record.get("license") != dep.license:
            problems.append(f"deps.json: {dep.name} was built under licence {record.get('license')!r}, "
                            f"wkpins says {dep.license!r} (rebuild)")
    return problems


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--rows", action="store_true", help="print the THIRD_PARTY.md rows and exit")
    args = parser.parse_args()
    if args.rows:
        print("\n".join(rows()))
        return 0
    nettls = nettls_licenses()
    problems = [p for dep in libraries() if (p := verdict(dep, nettls)[2])]
    names = {dep.name for dep in wkpins.DEPS}
    problems += [f"NEVER_IN_LAZYWEB names {n!r}, which is not a pinned library"
                 for n in wkpins.NEVER_IN_LAZYWEB if n not in names]
    third_party = THIRD_PARTY.read_text(encoding="utf-8") if THIRD_PARTY.is_file() else ""
    problems += check_register(third_party)
    problems += check_notices(third_party, README.read_text(encoding="utf-8"))
    problems += check_lazyweb_apart()
    problems += check_webplus_licence()
    print("\n".join(table(nettls)))
    if MANIFEST.is_file():
        problems += check_manifest(json.loads(MANIFEST.read_text(encoding="utf-8")))
    else:
        print("(no target/webkit/deps.json: the build's recorded licences were not compared)")
    if problems:
        print("licence problems:")
        for line in problems:
            print(f"  {line}")
        return 1
    print(f"{len(libraries())} libraries and {len(wkpins.DATA_FILES)} embedded data file(s): "
          "every licence is acceptable for an LGPL-2.1 binary; "
          f"GPLv2-incompatible, never in LazyWeb: {', '.join(wkpins.NEVER_IN_LAZYWEB)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
