"""The LazyWeb harness's throwaway certificates (`tools/net/tlscerts.py`'s CA
and leaf builders): a test CA the image trusts through `LAZYOS_TLS_TEST_CA`,
and one leaf for theoldnet.com, www.theoldnet.com and the Wikipedia hosts
(`wiki.HOSTS`). They live only in the
run's output directory; no key is ever committed.

The hosts file maps the stand-in sites' names to the host as the guest sees
it, for `LAZYOS_TLS_TEST_HOSTS`.
"""

from __future__ import annotations

import datetime
import sys
from dataclasses import dataclass
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent / "net"))
import tlscerts  # noqa: E402

sys.path.insert(0, str(HERE))
from wiki import HOSTS as WIKI_HOSTS  # noqa: E402

#: The names the leaf covers; the first is the subject.
LEAF_NAMES = ["theoldnet.com", "www.theoldnet.com", *WIKI_HOSTS]
#: Every name the test image maps to the host.
HOST_NAMES = ["example.com", "www.example.com", "theoldnet.com", "www.theoldnet.com",
              *WIKI_HOSTS]
#: The host, from the guest on QEMU's user network.
GATEWAY = "10.0.2.2"


@dataclass
class Files:
    ca: Path
    leaf_cert: Path
    leaf_key: Path
    hosts: Path


def _files(out_dir: Path) -> Files:
    return Files(out_dir / "ca.pem", out_dir / "theoldnet.pem", out_dir / "theoldnet.key",
                 out_dir / "hosts")


def hosts_text() -> str:
    """The test lines appended to the image's /etc/hosts."""
    return f"{GATEWAY} {' '.join(HOST_NAMES)}\n"


def generate(out_dir: Path) -> Files:
    """A fresh CA, leaf and hosts file under `out_dir`."""
    tlscerts.require()
    out_dir.mkdir(parents=True, exist_ok=True)
    files = _files(out_dir)
    now = datetime.datetime.now(datetime.timezone.utc)
    ca, ca_key = tlscerts._ca("LazyOS LazyWeb harness test CA", now)
    tlscerts._write(files.ca.with_suffix(""), ca, ca_key)
    leaf, key = tlscerts._leaf(LEAF_NAMES, ca, ca_key, now - tlscerts.DAY, now + 20 * tlscerts.DAY)
    tlscerts._write(files.leaf_cert.with_suffix(""), leaf, key)
    # The CA's key is not needed again; leave nothing that could sign.
    files.ca.with_suffix(".key").unlink(missing_ok=True)
    files.hosts.write_text(hosts_text())
    return files


def load(out_dir: Path) -> Files | None:
    """The files a previous `generate` wrote (an image built with that CA must
    meet the same leaf), or None if any is missing."""
    files = _files(out_dir)
    present = all(p.is_file() for p in (files.ca, files.leaf_cert, files.leaf_key, files.hosts))
    return files if present else None
