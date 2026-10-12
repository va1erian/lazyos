#!/usr/bin/env python3
"""Make the netboot signing key pair (docs/netboot-plan.md section 4, decision 4).

    python tools/netboot/keygen.py [--out target/netboot] [--name "dev laptop"] [--force]

Writes `key.sec` (the 32-byte seed, 64 hex characters, mode 0600 where the OS
has modes) and `key.pub`, and prints the key id and the `keys.json` entry the
registry needs. The secret belongs on the trusted dev machine only: it signs
every build and every write to the registry, and `ed25519.py` signs in
pure Python without any side-channel care. `target/` is git-ignored; never
copy `key.sec` to the homelab.
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))  # `python -I` drops the script directory

import ed25519  # noqa: E402
import keys  # noqa: E402

DEFAULT_OUT = HERE.parent.parent / "target" / "netboot"


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--out", default=str(DEFAULT_OUT), help="directory for key.sec and key.pub (default %(default)s)")
    ap.add_argument("--name", help="free text for the keys.json entry (default: this host's name)")
    ap.add_argument("--force", action="store_true", help="replace an existing key pair")
    args = ap.parse_args(argv)
    out = Path(args.out)
    if (out / "key.sec").exists() and not args.force:
        print(f"keygen: {out / 'key.sec'} exists (use --force to replace it; builds signed "
              "with the old key stop being pushable)", file=sys.stderr)
        return 1
    seed = ed25519.generate_seed()
    sec, pub_path = keys.write_keypair(out, seed)
    pub = ed25519.public_key(seed)
    print(f"wrote {sec} (keep it secret) and {pub_path}")
    print(f"key id {keys.key_id(pub)}")
    print("registry keys.json entry (or: push.py --register-key):")
    print(json.dumps(keys.keys_entry(pub, args.name)))
    return 0


if __name__ == "__main__":
    sys.exit(main())
