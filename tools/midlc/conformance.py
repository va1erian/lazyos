#!/usr/bin/env python3
"""Regenerate or check the MIDL conformance corpus (`idl/conformance/`).

    python tools/midlc/conformance.py --write   # after changing a case or midlc
    python tools/midlc/conformance.py --check   # CI: fail if anything would change

`--check` fails when an expected file, or the Rust test that holds the
generated codecs to it (`libs/generated/tests/conformance.rs`), differs from
what `midlc` produces now; `cargo test -p messenger-generated` then runs that
test. See `docs/midl.md` ("Conformance corpus").
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from midlc_conformance import expected_files, stale_expected  # noqa: E402
from midlc_conformance_rust import emit_conformance_rust  # noqa: E402
from midlc_model import MidlError  # noqa: E402

ROOT = Path(__file__).resolve().parents[2]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--write", action="store_true", help="write the expected files and the Rust test")
    mode.add_argument("--check", action="store_true", help="fail if --write would change anything")
    parser.add_argument("--corpus", type=Path, default=ROOT / "idl" / "conformance")
    parser.add_argument("--rust", type=Path, default=ROOT / "libs" / "generated" / "tests" / "conformance.rs")
    args = parser.parse_args()

    try:
        files = expected_files(args.corpus)
        files[args.rust] = emit_conformance_rust(args.corpus, files)
    except MidlError as error:
        print(f"conformance: {error}", file=sys.stderr)
        return 1
    stale = stale_expected(args.corpus, files)

    if args.write:
        for path in stale:
            path.unlink()
        for path, text in files.items():
            path.write_text(text, encoding="utf-8", newline="\n")
        print(f"conformance: wrote {len(files)} file(s)")
        return 0

    failed = [f"{path} has no .midl any more" for path in stale]
    for path, text in files.items():
        if not path.is_file() or path.read_text(encoding="utf-8") != text:
            failed.append(f"{path} is out of date")
    for line in failed:
        print(f"conformance: {line}", file=sys.stderr)
    if failed:
        print("conformance: regenerate with: python tools/midlc/conformance.py --write", file=sys.stderr)
        return 1
    print(f"conformance: {len(files)} file(s) up to date")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
