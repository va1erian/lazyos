"""The guest paths the bundle bakes into its libraries, read from ``libs/fhs``.

Fontconfig, OpenSSL and curl compile the places they look into their static
archives, and ``depsproof`` repeats two of them, so a path that drifts from
``libs/fhs`` would be wrong silently until a font or a certificate is not
found on LazyOS. There is one source: this module parses the ``pub const``
strings of the ``fhs`` crate (``python tools/fhs/check_literals.py`` keeps Rust
literals out; this does the same for the build scripts and the C++ proof), and
``test_paths.py`` fails when a recipe or a source file spells one out.

    fhspaths.get("share::FONTS")  ->  "/system/share/fonts"
"""

from __future__ import annotations

import re
from pathlib import Path

FHS = Path(__file__).resolve().parent.parent.parent / "libs" / "fhs" / "src"
CONST = re.compile(r'^\s*pub const (\w+): &str = "([^"]*)";', re.M)


def constants(module: str) -> dict[str, str]:
    """Every ``pub const NAME: &str = "..."`` of ``libs/fhs/src/<module>.rs``."""
    return dict(CONST.findall((FHS / f"{module}.rs").read_text(encoding="utf-8")))


def get(path: str) -> str:
    """The value of ``<module>::<NAME>``; KeyError names what is missing."""
    module, name = path.split("::")
    return constants(module)[name]


#: The names below are the whole list of paths the bundle bakes in.
FONT_DIR = get("share::FONTS")
FONT_CONF_DIR = get("system::SYSTEM_ETC")
FONT_CACHE_DIR = get("state::FONTCONFIG_CACHE")
OPENSSL_DIR = get("etc::LINUX_SSL")
CA_BUNDLE = get("etc::LINUX_CA_BUNDLE")
TMP = get("mount::TMP")

#: What ``proof.py`` hands the C++ proof as ``-D`` macros (checked in ``depsproof/proof.h``).
PROOF_DEFINES = {"DEPSPROOF_CA_BUNDLE": CA_BUNDLE, "DEPSPROOF_FONT_CACHE_DIR": FONT_CACHE_DIR,
                 "DEPSPROOF_TMP": TMP}
