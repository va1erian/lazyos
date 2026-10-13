#!/usr/bin/env python3
"""A fixed subset of WebKit's ``JSTests/stress`` for the guest (docs/webkit-port-plan.md W2).

``JSTests/stress`` holds thousands of self-contained scripts that throw when
they fail; the harness that runs them (``Tools/Scripts/run-jsc-stress-tests``)
is Ruby and cannot run in the guest, and the WebKitGTK release tarball does not
carry ``JSTests``. This script fetches ``JSTests/stress`` at the commit the
``webkitgtk-2.54.1`` tag points to (git objects are content-addressed, so the
commit is the pin) with a sparse, blob-less clone, picks a subset by a rule
that depends on nothing but the file name and text, and writes it to
``target/jsc/jsc-tests/`` with an ``index.txt``. ``LAZYOS_JSC=1`` embeds that
directory at ``/system/share/jsc-tests/`` and the ``jsc`` fixture runs every
file in it (``JSC:STRESS:<pass>/<total>``).

The rule (:func:`selected`):

* a flat ``*.js`` file of ``JSTests/stress`` whose name starts with one of
  :data:`LETTERS`;
* at most :data:`MAX_BYTES` long;
* that none of :data:`EXCLUDED` matches: marked ``//@ skip``, needing options
  (``//@ requireOptions``), the ``$vm`` object, WebAssembly (off in this build),
  the test runner, ``load``/``readFile`` of resources, modules, ``$262`` or
  ``$.agent``.

The harness's ``//@ run...`` lines pick option sets to run a file under; here
every file runs once with the shell's defaults, the one configuration all of
them are meant to pass in.

Usage::

    python tools/jsc/stress.py            # fetch (once) and write the bundle
    python tools/jsc/stress.py --list     # the selected names
"""

from __future__ import annotations

import argparse
import os
import re
import shutil
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import pins  # noqa: E402

REPO = "https://github.com/WebKit/WebKit.git"
#: The commit the `webkitgtk-2.54.1` tag points to (`git ls-remote --tags`).
COMMIT = "628d6ce1850e17225185a5807a9d0aa375bae2be"
#: What the checkout's marker file holds once it is done: the commit, fetched
#: without line-end conversion (an older checkout, made with the user's
#: `core.autocrlf`, is fetched again).
FETCHED = COMMIT + " raw"
CHECKOUT = pins.ROOT / "target" / "jsc-work" / "jstests"
STRESS = CHECKOUT / "JSTests" / "stress"
#: What `LAZYOS_JSC=1` embeds, and the fixture reads.
BUNDLE = pins.OUT.parent / "jsc-tests"

#: First letters of the selected names: ``g`` (generators, getters, globals) and
#: ``n`` (new, number, null, ...), 255 files that run in seconds in the guest.
LETTERS = "gn"
MAX_BYTES = 40_000
#: Text that makes a file unfit for a bare ``jsc file.js`` (or for this build).
EXCLUDED = [
    r"//@\s*skip",
    r"//@\s*requireOptions",
    r"//@\s*slow",
    r"\$vm",
    r"WebAssembly",
    r"Wasm",
    r"testRunner",
    r"\bload\(",
    r"readFile",
    r"\bimport\b",
    r"\bexport\b",
    r"\$262",
    r"\$\.agent",
]
_EXCLUDED = re.compile("|".join(f"(?:{pattern})" for pattern in EXCLUDED))


def fit(name: str, data: bytes) -> bool:
    """Whether the rule selects the file `name` with contents `data`.

    The exclusions match on a lossy UTF-8 decoding; the bytes themselves are
    never changed (a test may hold CRLF line ends or invalid UTF-8 on purpose).
    """
    return (name.endswith(".js") and name[:1] in LETTERS and len(data) <= MAX_BYTES
            and _EXCLUDED.search(data.decode("utf-8", errors="replace")) is None)


def selected(directory: Path = STRESS) -> list[tuple[str, bytes]]:
    """``(name, bytes)`` of every selected file, sorted by name."""
    found = []
    for path in sorted(directory.glob("*.js")):
        data = path.read_bytes()
        if fit(path.name, data):
            found.append((path.name, data))
    return found


def remove_tree(path: Path) -> None:
    """`rmtree` that also removes git's read-only object files (Windows)."""
    def writable(function, name, _error):
        os.chmod(name, 0o700)
        function(name)
    if path.exists():
        try:
            shutil.rmtree(path, onexc=writable)  # Python 3.12+
        except TypeError:
            shutil.rmtree(path, onerror=writable)


def git(*args: str, cwd: Path) -> None:
    subprocess.run(["git", *args], cwd=cwd, check=True)


def fetch() -> Path:
    """`JSTests/stress` at :data:`COMMIT`, cloned once (sparse, no blobs but the ones wanted)."""
    marker = CHECKOUT / ".lazyos-fetched"
    if marker.is_file() and marker.read_text().strip() == FETCHED:
        return STRESS
    remove_tree(CHECKOUT)
    CHECKOUT.mkdir(parents=True)
    git("init", "-q", cwd=CHECKOUT)
    # Byte-exact files: no CRLF conversion on a Windows checkout.
    git("config", "core.autocrlf", "false", cwd=CHECKOUT)
    git("remote", "add", "origin", REPO, cwd=CHECKOUT)
    git("sparse-checkout", "set", "--cone", "JSTests/stress", cwd=CHECKOUT)
    git("fetch", "-q", "--depth", "1", "--filter=blob:none", "origin", COMMIT, cwd=CHECKOUT)
    git("checkout", "-q", COMMIT, cwd=CHECKOUT)
    head = subprocess.run(["git", "rev-parse", "HEAD"], cwd=CHECKOUT, check=True,
                          capture_output=True, text=True).stdout.strip()
    if head != COMMIT:
        raise SystemExit(f"stress: checked out {head}, wanted {COMMIT}")
    marker.write_text(FETCHED + "\n")
    return STRESS


def bundle(out: Path = BUNDLE) -> list[str]:
    """Write the selected files and `index.txt` to `out`; the names."""
    files = selected(fetch())
    remove_tree(out)
    out.mkdir(parents=True)
    for name, data in files:
        (out / name).write_bytes(data)
    names = [name for name, _ in files]
    (out / "index.txt").write_text("".join(f"{name}\n" for name in names), newline="\n")
    return names


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--list", action="store_true", help="print the selected names")
    args = parser.parse_args()
    names = bundle()
    if args.list:
        print("\n".join(names))
    total = sum((BUNDLE / name).stat().st_size for name in names)
    pins.log(f"{len(names)} stress tests, {total / 1024:.0f} KiB in {BUNDLE}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
