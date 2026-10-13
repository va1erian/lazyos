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
OUTPUTS = {"WebProcess": "WebProcess", "NetworkProcess": "NetworkProcess", "webkit-smoke": "webkit-smoke",
           "webkit-app": "webkit-app", "lazywebplus-app": "lazywebplus-app"}
#: The windows' static libraries (``tools/webkit/app.py`` builds them on the host); with one the
#: ``webkit-app`` or ``lazywebplus-app`` target exists (``Source/WebKit/PlatformLazyOS.cmake``).
UI_LIBRARY = wkpins.OUT / "lib" / "libxui_webkit.a"
PLUS_LIBRARY = wkpins.OUT / "lib" / "libxui_webplus.a"
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
    window = {"LAZYOS_UI_LIBRARY": str(UI_LIBRARY)} if UI_LIBRARY.is_file() else {}
    if PLUS_LIBRARY.is_file():
        window["LAZYOS_PLUS_LIBRARY"] = str(PLUS_LIBRARY)
    return {
        **window,
        "PORT": "LazyOS", "CMAKE_BUILD_TYPE": "Release", "DEVELOPER_MODE": "OFF",
        "ENABLE_TOOLS": "OFF", "ENABLE_MINIBROWSER": "OFF", "USE_HEADER_MAPS": "OFF",
        # The Notifications API (docs/lazywebplus.md): on, explicitly, so a build tree configured
        # before it was (the cache keeps the first default) is configured again.
        "ENABLE_NOTIFICATIONS": "ON",
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
        # The guest paths of libs/fhs the C++ is compiled with (the sources refuse to build without).
        **fhspaths.WEBKIT_DEFINES,
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
    write_fonts_conf()
    sizes = {name: Path(path).stat().st_size for name, path in found.items()}
    (wkpins.OUT / "port.json").write_text(json.dumps({"sizes": sizes, "built_at": time.time()}, indent=1))
    return found


def write_fonts_conf() -> Path:
    """``target/webkit/fonts.conf``: the template with the paths of ``libs/fhs`` (the image embeds it)."""
    wkpins.OUT.mkdir(parents=True, exist_ok=True)
    FONTS_CONF.write_text(fhspaths.fonts_conf((PORT_DIR / "fonts.conf.in").read_text(encoding="utf-8")),
                          encoding="utf-8")
    return FONTS_CONF


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
FONTS_CONF = wkpins.OUT / "fonts.conf"
#: The image keeps program names lowercase (libs/fhs); the build names follow WebKit.
IMAGE_NAMES = {"WebProcess": "webprocess", "NetworkProcess": "networkprocess"}


def install_host_layout() -> None:
    """Lays the image's programs and fonts down at their ``/system`` paths of the build container,
    so a built program runs there as it does on LazyOS (``host_reference``, ``blockcheck.py``)."""
    import shutil
    for directory in (HOST_BIN, HOST_FONTS, HOST_FONTS_CONF.parent):
        directory.mkdir(parents=True, exist_ok=True)
    for name in OUTPUTS:
        if not (wkpins.BIN / name).is_file():
            continue  # the desktop apps are built on request (`app.py`), after this
        installed = HOST_BIN / IMAGE_NAMES.get(name, name)
        shutil.copy2(wkpins.BIN / name, installed)
        installed.chmod(0o755)
    for font in (wkpins.ROOT / "assets" / "fonts" / "liberation").glob("*.ttf"):
        shutil.copy2(font, HOST_FONTS / font.name)
    shutil.copy2(write_fonts_conf(), HOST_FONTS_CONF)
    shutil.rmtree(fhspaths.FONT_CACHE_DIR, ignore_errors=True)


def host_reference() -> bool:
    """Run ``webkit-smoke`` once on the build host and keep what it printed.

    The programs are static musl executables, so a Linux host runs them as they are; the guest
    must paint the same bitmap (``portjudge`` compares the checksum and the glyphs' ink). Only
    inside the build container: it lays the image's ``/system`` paths down, which no developer's
    own machine should get. True when the run passed and ``REFERENCE`` was written; False when
    it did not run at all (not in the container). A run that fails or times out is a
    ``BuildError`` and leaves no reference file (a stale one would be judged against).

    The run happens in a fresh directory of the work volume: the NetworkProcess keeps its cookie
    jar (``cookie.jar.db``) in the working directory it inherits, which must not be the bind-mounted
    repository.
    """
    import shutil
    import subprocess
    import tempfile
    REFERENCE.unlink(missing_ok=True)
    if os.environ.get("LAZYOS_JSC_IN_CONTAINER") != "1":
        wkpins.log("not in the build container: no host reference run")
        return False
    install_host_layout()
    wkpins.jsc_pins.WORK.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="webkit-smoke-", dir=wkpins.jsc_pins.WORK,
                                     ignore_cleanup_errors=True) as scratch:
        try:
            done = subprocess.run([str(HOST_BIN / "webkit-smoke")], capture_output=True, text=True,
                                  timeout=300, cwd=scratch)
        except subprocess.TimeoutExpired as error:
            raise BuildError("the host reference run of webkit-smoke timed out after 300 s:\n"
                             + _tail(error.stdout)) from error
    if done.returncode != 0 or "WEBKIT:W4:PASS" not in done.stdout:
        raise BuildError(f"the host reference run of webkit-smoke failed (exit {done.returncode}):\n"
                         + done.stdout[-1500:] + done.stderr[-500:])
    REFERENCE.write_text(done.stdout)
    wkpins.log("host reference run passed")
    return True


def _tail(output: str | bytes | None) -> str:
    if isinstance(output, bytes):
        output = output.decode(errors="replace")
    return (output or "")[-1500:]
