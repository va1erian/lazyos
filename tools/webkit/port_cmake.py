"""Configure, build and collect the WebKit port (W4): WebProcess, NetworkProcess
and the headless UI-process program ``webkit-smoke``.

WebKit's own CMake does the work (``PORT=LazyOS``, ``Source/cmake/OptionsLazyOS.cmake``
laid into the tree by ``port_tree.py``); this module only chooses the compiler
(zig, as in ``tools/jsc/webkit.py``), points CMake at the dependency bundle's
prefix and runs ninja. The build tree lives in the work volume; rebuilds are
incremental.
"""

from __future__ import annotations

import json
import os
import time
from pathlib import Path

import buildkit
import toolenv
import fhspaths
import wkpins
from toolenv import BuildError, run

BUILD = wkpins.jsc_pins.WORK / "webkit-port-build"
PREFIX = wkpins.PREFIX
PORT_DIR = wkpins.ROOT / "webkit" / "port"
OUTPUTS = {"WebProcess": "WebProcess", "NetworkProcess": "NetworkProcess", "webkit-smoke": "webkit-smoke"}
#: Static archives of the bundle, in an order a single-pass linker would accept
#: (every archive after the ones that use it). lld does not need the order.
STATIC_LIBRARIES = [
    "libcurl.a", "libnghttp2.a", "libssl.a", "libcrypto.a", "libpsl.a", "libbrotlidec.a",
    "libbrotlicommon.a", "libxml2.a", "libsqlite3.a", "libwebpdemux.a", "libwebp.a", "libsharpyuv.a",
    "libwpe-1.0.a", "libfontconfig.a", "libfreetype.a", "libharfbuzz-icu.a", "libharfbuzz.a",
    "libpng16.a", "libjpeg.a", "libexpat.a", "libicui18n.a", "libicuuc.a", "libicudata.a",
    "libEGL.a", "libGLESv2.a", "libz.a",
]


def simd_flags() -> str:
    """simdutf's AVX paths need `evex512` and LazyOS enables no AVX state (tools/jsc/webkit.py)."""
    return " ".join(f"-DSIMDUTF_IMPLEMENTATION_{n}=0" for n in ("ICELAKE", "HASWELL", "WESTMERE"))


def options() -> dict[str, str]:
    wrap = toolenv.wrappers()
    flags = " ".join(toolenv.CODEGEN)
    libs = ";".join(str(PREFIX / "lib" / name) for name in STATIC_LIBRARIES)
    return {
        "PORT": "LazyOS", "CMAKE_BUILD_TYPE": "Release", "DEVELOPER_MODE": "OFF",
        "ENABLE_TOOLS": "OFF", "ENABLE_MINIBROWSER": "OFF", "USE_HEADER_MAPS": "OFF",
        "CMAKE_C_COMPILER": wrap["zcc"], "CMAKE_CXX_COMPILER": wrap["zc++"],
        "CMAKE_AR": wrap["zar"], "CMAKE_RANLIB": wrap["zranlib"],
        "CMAKE_C_FLAGS": flags, "CMAKE_CXX_FLAGS": f"{flags} {simd_flags()} -std=c++2b",
        "CMAKE_C_FLAGS_RELEASE": "-DNDEBUG", "CMAKE_CXX_FLAGS_RELEASE": "-DNDEBUG",
        "CMAKE_EXE_LINKER_FLAGS": " ".join(toolenv.LINK),
        "ICU_ROOT": str(PREFIX), "CMAKE_PREFIX_PATH": str(PREFIX),
        "CMAKE_FIND_ROOT_PATH": str(PREFIX),
        # Libraries and headers come from the bundle only; programs (perl, ruby, gperf) from the host.
        "CMAKE_FIND_ROOT_PATH_MODE_LIBRARY": "ONLY", "CMAKE_FIND_ROOT_PATH_MODE_INCLUDE": "ONLY",
        "CMAKE_FIND_ROOT_PATH_MODE_PROGRAM": "NEVER",
        "LAZYOS_STATIC_LIBRARIES": libs, "LAZYOS_PORT_DIR": str(PORT_DIR),
    }


def configure(tree: Path) -> None:
    BUILD.mkdir(parents=True, exist_ok=True)
    command = ["cmake", "-G", "Ninja", "-S", str(tree), "-B", str(BUILD),
               *[f"-D{key}={value}" for key, value in options().items()]]
    stamp = BUILD / ".lazyos-configured"
    wanted = " ".join(command)
    if (BUILD / "build.ninja").is_file() and stamp.is_file() and stamp.read_text() == wanted:
        return
    run(command, BUILD, "webkit port configure", env=buildkit.pkg_env())
    stamp.write_text(wanted)


def build(tree: Path, jobs: int, targets: list[str]) -> dict[str, str]:
    """Run ninja for `targets`; strip the programs into ``OUT/bin``; name to path."""
    configure(tree)
    began = time.monotonic()
    run(["ninja", "-C", str(BUILD), f"-j{jobs}", "-k", "0", *targets], BUILD, "webkit port build",
        env=buildkit.pkg_env())
    wkpins.log(f"ninja finished in {time.monotonic() - began:.0f} s")
    return collect(targets)


def collect(targets: list[str]) -> dict[str, str]:
    found: dict[str, str] = {}
    wkpins.BIN.mkdir(parents=True, exist_ok=True)
    for name in targets:
        built = BUILD / "bin" / OUTPUTS.get(name, name)
        if not built.is_file():
            raise BuildError(f"ninja finished but {built} is missing")
        out = wkpins.BIN / name
        run(["strip", "--strip-all", "-o", str(out), str(built)], BUILD, f"strip {name}")
        out.chmod(0o755)
        found[name] = str(out)
    sizes = {name: Path(path).stat().st_size for name, path in found.items()}
    (wkpins.OUT / "port.json").write_text(json.dumps({"sizes": sizes, "built_at": time.time()}, indent=1))
    return found


def chown_outputs() -> None:
    uid, gid = os.environ.get("LAZYOS_JSC_UID"), os.environ.get("LAZYOS_JSC_GID")
    if uid is None or gid is None:
        return
    for path in [wkpins.OUT, *wkpins.OUT.rglob("*")]:
        os.chown(path, int(uid), int(gid))


#: Where the programs expect to find each other and their fonts (the image's layout).
HOST_BIN = Path(fhspaths.get("system::SYSTEM_BIN"))
HOST_FONTS = Path(fhspaths.get("share::FONTS_LIBERATION"))
HOST_FONTS_CONF = Path(fhspaths.get("etc::FONTS_CONF"))
REFERENCE = wkpins.OUT / "smoke.host.txt"
#: The image keeps program names lowercase (libs/fhs); the build names follow WebKit.
IMAGE_NAMES = {"WebProcess": "webprocess", "NetworkProcess": "networkprocess"}


def host_reference() -> bool:
    """Run ``webkit-smoke`` once on the build host and keep what it printed.

    The programs are static musl executables, so a Linux host runs them as they are; the guest
    must paint the same bitmap (``portjudge`` compares the checksum and the glyphs' ink). Only
    inside the build container: it lays the image's ``/system`` paths down, which no developer's
    own machine should get. True when the run passed.
    """
    import shutil
    import subprocess
    if os.environ.get("LAZYOS_JSC_IN_CONTAINER") != "1":
        wkpins.log("not in the build container: no host reference run")
        return False
    for directory in (HOST_BIN, HOST_FONTS, HOST_FONTS_CONF.parent):
        directory.mkdir(parents=True, exist_ok=True)
    for name in OUTPUTS:
        installed = HOST_BIN / IMAGE_NAMES.get(name, name)
        shutil.copy2(wkpins.BIN / name, installed)
        installed.chmod(0o755)
    for font in (wkpins.ROOT / "assets" / "fonts" / "liberation").glob("*.ttf"):
        shutil.copy2(font, HOST_FONTS / font.name)
    shutil.copy2(PORT_DIR / "fonts.conf", HOST_FONTS_CONF)
    shutil.rmtree(fhspaths.FONT_CACHE_DIR, ignore_errors=True)
    done = subprocess.run([str(HOST_BIN / "webkit-smoke")], capture_output=True, text=True, timeout=300)
    REFERENCE.write_text(done.stdout)
    ok = done.returncode == 0 and "WEBKIT:W4:PASS" in done.stdout
    wkpins.log("host reference run " + ("passed" if ok else "FAILED:\n" + done.stdout[-1500:] + done.stderr[-500:]))
    return ok
