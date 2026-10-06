#!/usr/bin/env python3
"""Write the small, checked-in seed set for the cargo-fuzz targets.

The seeds are hand-shaped scripts in each target's byte grammar (see
`libs/framering/src/fuzz.rs` and `libs/virtio-net/src/fuzz.rs`), so libFuzzer
starts from inputs that already reach the interesting paths - a full ring, an
index wrap, every kind of scribble, a good and a hostile completion - instead
of rediscovering the grammar from noise. The `lazypkg` target takes real `.lzp`
zip archives instead, built here with `struct`/`zlib` so the bytes are
deterministic across Python versions (Python's `zipfile` output is not).

    python fuzz/gen_corpus.py            # rewrite fuzz/seeds/
    python fuzz/gen_corpus.py --check    # fail if the checked-in seeds differ or extra files appear

Everything here is deterministic; the files are plain bytes.
"""
import argparse
import sys
from pathlib import Path

# The seed builders, one module per family of targets beside this script.
from seeds_accounts import accountwire_seeds, passwd_seeds, pkgstore_rules_seeds
from seeds_formats import ipp_seeds, lazypkg_seeds, pwgraster_seeds
from seeds_input import hidreport_seeds, hidreportdesc_seeds, inputmap_pointer_seeds, usbdesc_seeds
from seeds_net import framering_seeds, header_seeds, netstack_seeds, nicdrv_seeds, virtio_net_seeds
from seeds_storage import (
    acpi_seeds, ext2fs_seeds, mscdesc_seeds, mscreply_seeds, mscsession_seeds, nvme_seeds,
)

ROOT = Path(__file__).resolve().parent / "seeds"


TARGETS = {
    "acpi": acpi_seeds,
    "ext2fs": ext2fs_seeds,
    "framering": framering_seeds,
    "framering_header": header_seeds,
    "virtio_net": virtio_net_seeds,
    "nicdrv": nicdrv_seeds,
    "lazypkg": lazypkg_seeds,
    "netstack": netstack_seeds,
    "inputmap_pointer": inputmap_pointer_seeds,
    "usbdesc": usbdesc_seeds,
    "hidreport": hidreport_seeds,
    "hidreportdesc": hidreportdesc_seeds,
    "mscdesc": mscdesc_seeds,
    "mscreply": mscreply_seeds,
    "mscsession": mscsession_seeds,
    "ipp": ipp_seeds,
    "pwgraster": pwgraster_seeds,
    "nvme": nvme_seeds,
    "passwd": passwd_seeds,
    "accountwire": accountwire_seeds,
    "pkgstore_rules": pkgstore_rules_seeds,
}


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--check", action="store_true", help="fail if the checked-in corpus differs")
    args = parser.parse_args()
    bad = False
    for target, make in TARGETS.items():
        directory = ROOT / target
        for name, data in make().items():
            path = directory / name
            if args.check:
                if not path.exists() or path.read_bytes() != data:
                    print(f"stale: {path}")
                    bad = True
            else:
                directory.mkdir(parents=True, exist_ok=True)
                path.write_bytes(data)
    if args.check:
        # Anything else in seeds/ is libFuzzer output that leaked in; the
        # working corpus lives in the git-ignored fuzz/corpus/.
        wanted = {ROOT / target / name for target, make in TARGETS.items() for name in make()}
        for path in sorted(ROOT.rglob("*")):
            if path.is_file() and path not in wanted:
                print(f"unexpected: {path}")
                bad = True
        return 1 if bad else 0
    print(f"wrote the seeds under {ROOT}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
