#!/usr/bin/env python3
"""Write or check LazyVM's Java conformance test and the classes it uses.

    python tools/midlc/conformance_java.py --test T.java --gen DIR [--check]

`--test` is the test source (LazyVM's `tests/java/lazy/msg/Conformance.java`),
`--gen` the directory the generated classes of the corpus interfaces go under
(LazyVM's `tests/msg/gen`). See `midlc_conformance_java.py`.
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from midlc_conformance_java import emit_conformance_java  # noqa: E402
from midlc_model import MidlError  # noqa: E402

ROOT = Path(__file__).resolve().parents[2]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--test", type=Path, required=True)
    parser.add_argument("--gen", type=Path, required=True)
    parser.add_argument("--package", default="lazy.msg")
    parser.add_argument("--corpus", type=Path, default=ROOT / "idl" / "conformance")
    parser.add_argument("--check", action="store_true", help="fail if writing would change anything")
    args = parser.parse_args()
    try:
        source, classes = emit_conformance_java(args.corpus, args.package, args.test.stem)
    except MidlError as error:
        print(f"conformance_java: {error}", file=sys.stderr)
        return 1
    files = {args.test: source, **{args.gen / name: text for name, text in classes.items()}}
    if args.check:
        stale = [p for p, text in files.items() if not p.is_file() or p.read_text(encoding="utf-8") != text]
        for path in stale:
            print(f"conformance_java: {path} is out of date", file=sys.stderr)
        return 1 if stale else 0
    for path, text in files.items():
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8", newline="\n")
    print(f"conformance_java: wrote {len(files)} file(s)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
