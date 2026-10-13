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

PROGRAM = re.compile(r'^\s*(\w+) = "([^"]+)";', re.M)


def program(name: str) -> str:
    """The image path of a program named in ``libs/fhs/src/bin.rs`` (``WEBPROCESS`` ->
    ``/system/bin/webprocess``)."""
    names = dict(PROGRAM.findall((FHS / "bin.rs").read_text(encoding="utf-8")))
    return f"{get('system::SYSTEM_BIN')}/{names[name]}"


MANIFEST = Path(__file__).resolve().parent.parent.parent / "xui-app" / "packages" / "webkit" / "manifest.toml"


def webkit_app_id() -> str:
    """The browser package's ``system_name`` (its per-user folder is ``<home>/.apps/<system_name>``)."""
    found = re.search(r'^system_name\s*=\s*"([^"]+)"', MANIFEST.read_text(encoding="utf-8"), re.M)
    if not found:
        raise KeyError(f"{MANIFEST} has no system_name")
    return found.group(1)


#: What the WebKit port's C++ gets as ``-D`` macros (``port_cmake.options``; the sources refuse to
#: compile without them): the programs the launcher and the smoke program start, the CA bundle,
#: and the two names of the browser's profile directory (``<home>/.apps/os.lazy.webkit``).
WEBKIT_DEFINES = {"LAZYOS_WEBPROCESS_PATH": program("WEBPROCESS"),
                  "LAZYOS_NETWORKPROCESS_PATH": program("NETWORKPROCESS"),
                  "LAZYOS_CA_BUNDLE_PATH": CA_BUNDLE,
                  "LAZYOS_APP_DATA_DIR": get("state::APP_DATA_DIR"),
                  "LAZYOS_WEBKIT_APP_ID": webkit_app_id()}
#: The ``@NAME@`` paths of ``webkit/port/fonts.conf.in``.
FONTS_CONF_VALUES = {"FONT_DIR": FONT_DIR, "FONT_CACHE_DIR": FONT_CACHE_DIR}


def fonts_conf(template: str) -> str:
    """`template` with its ``@NAME@`` placeholders filled in; a placeholder left over is an error."""
    text = template
    for name, value in FONTS_CONF_VALUES.items():
        text = text.replace(f"@{name}@", value)
    left = re.findall(r"@\w+@", text)
    if left:
        raise KeyError(f"fonts.conf.in has placeholders without a value: {left}")
    return text


#: What ``proof.py`` hands the C++ proof as ``-D`` macros (checked in ``depsproof/proof.h``).
PROOF_DEFINES = {"DEPSPROOF_CA_BUNDLE": CA_BUNDLE, "DEPSPROOF_FONT_CACHE_DIR": FONT_CACHE_DIR,
                 "DEPSPROOF_TMP": TMP}
