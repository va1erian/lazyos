#!/usr/bin/env python3
"""Check that every crate linked into `libs/smbwire` (and so into `smb` and,
later, `smbfuse`) has a GPLv2-compatible licence (docs/smb-plan.md §5).

The rule and the SPDX evaluation are `tools/nettls/licenses.py`'s, applied to
the SMB library for the target the OS programs are built for: `md4`, `md-5`,
`hmac` and `sha2` are `MIT OR Apache-2.0`, acceptable under the MIT choice, so
a later link into a GPL-2.0-only program stays clean.

    python tools/smb/licenses.py [--verbose]

Exit status 1 when any linked crate has no acceptable licence choice.
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent
sys.path.insert(0, str(ROOT / "tools" / "nettls"))
import licenses  # noqa: E402

MANIFEST = ROOT / "libs" / "smbwire" / "Cargo.toml"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--verbose", action="store_true", help="print every crate and its licence")
    args = parser.parse_args()
    licenses.self_test()
    # The OS programs' target, not the musl one the TLS clients use.
    licenses.TARGET = "x86_64-unknown-none"
    problems = licenses.check(MANIFEST, args.verbose)
    if problems:
        print("licences not GPLv2-compatible:")
        for line in problems:
            print(f"  {line}")
        return 1
    print("smbwire: all linked third-party crates have a GPLv2-compatible licence choice")
    return 0


if __name__ == "__main__":
    sys.exit(main())
