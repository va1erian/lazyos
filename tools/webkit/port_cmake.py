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
import zigfix
from toolenv import BuildError, run

BUILD = wkpins.jsc_pins.WORK / "webkit-port-build"
PREFIX = wkpins.PREFIX
PORT_DIR = wkpins.ROOT / "webkit" / "port"
OUTPUTS = {"WebProcess": "WebProcess", "NetworkProcess": "NetworkProcess", "webkit-smoke": "webkit-smoke",
           "lazywebplus-app": "lazywebplus-app", "webkit-views": "webkit-views",
           "webd-abi-test": "webd-abi-test", "webd-app": "webd-app"}
#: The window's static library (``tools/webkit/app.py`` builds it on the host); with it the
#: ``lazywebplus-app`` target exists (``Source/WebKit/PlatformLazyOS.cmake``).
PLUS_LIBRARY = wkpins.OUT / "lib" / "libxui_webplus.a"
#: webd's service, the same way (``tools/webkit/webd.py``): with it the ``webd-app`` target exists.
WEBD_LIBRARY = wkpins.OUT / "lib" / "libxui_webd_host.a"
#: Static archives of the bundle, in an order a single-pass linker would accept
#: (every archive after the ones that use it). lld does not need the order.
STATIC_LIBRARIES = [
    "libcurl.a", "libnghttp2.a", "libssl.a", "libcrypto.a", "libpsl.a", "libbrotlidec.a",
    "libbrotlicommon.a", "libxml2.a", "libsqlite3.a", "libwebpdemux.a", "libwebp.a", "libsharpyuv.a",
    "libwpe-1.0.a", "libfontconfig.a", "libfreetype.a", "libharfbuzz-icu.a", "libharfbuzz.a",
    "libpng16.a", "libjpeg.a", "libexpat.a", "libicui18n.a", "libicuuc.a", "libicudata.a",
    "libEGL.a", "libGLESv2.a", "libz.a",
]


#: WebKit is built -fno-exceptions and the programs never unwind (no backtrace, no catch), so the
#: unwind tables are dead weight: `.eh_frame`, its index and `.gcc_except_table` were 8.2 MB of
#: every 107 MiB program. (The bundle's libraries keep theirs: about 1.5 MB.)
NO_UNWIND = ["-fno-asynchronous-unwind-tables", "-fno-unwind-tables"]
#: lld folds functions and constants that are byte for byte the same, never one whose address
#: is taken (`-faddrsig`): 5.3 MB of NetworkProcess. (It warns for the three objects of the
#: bundle whose address-significance table objcopy damaged, and leaves those unfolded.)
#: zig's driver cannot pass `--icf`: the `zc++` wrapper links twice when this is set (toolenv.icf_wrapper).
LINK_ENV = {"LAZYOS_LINK_ICF": "1"}


def size_flags() -> str:
    return " ".join(NO_UNWIND)


def simd_flags() -> str:
    """simdutf's AVX paths need `evex512` and LazyOS enables no AVX state (tools/jsc/webkit.py)."""
    return " ".join(f"-DSIMDUTF_IMPLEMENTATION_{n}=0" for n in ("ICELAKE", "HASWELL", "WESTMERE"))


def options() -> dict[str, str]:
    wrap = toolenv.wrappers()
    flags = " ".join(toolenv.CODEGEN)
    libs = ";".join(str(PREFIX / "lib" / name) for name in STATIC_LIBRARIES)
    window = {"LAZYOS_PLUS_LIBRARY": str(PLUS_LIBRARY)} if PLUS_LIBRARY.is_file() else {}
    service = {"LAZYOS_WEBD_LIBRARY": str(WEBD_LIBRARY)} if WEBD_LIBRARY.is_file() else {}
    return {
        **window, **service,
        "PORT": "LazyOS", "CMAKE_BUILD_TYPE": "Release", "DEVELOPER_MODE": "OFF",
        "ENABLE_TOOLS": "OFF", "ENABLE_MINIBROWSER": "OFF", "USE_HEADER_MAPS": "OFF",
        # The Notifications API (docs/lazywebplus.md): on, explicitly, so a build tree configured
        # before it was (the cache keeps the first default) is configured again.
        "ENABLE_NOTIFICATIONS": "ON",
        # Content extensions (LazyWeb+'s ad blocker, #845): OptionsLazyOS.cmake's default is ON, but a
        # tree configured before it kept OFF and built a browser whose blocker has no store.
        "ENABLE_CONTENT_EXTENSIONS": "ON",
        "CMAKE_C_COMPILER": wrap["zcc"], "CMAKE_CXX_COMPILER": wrap["zc++"],
        "CMAKE_AR": wrap["zar"], "CMAKE_RANLIB": wrap["zranlib"],
        "CMAKE_C_FLAGS": f"{flags} {size_flags()}", "CMAKE_CXX_FLAGS": f"{flags} {size_flags()} {simd_flags()} -std=c++2b",
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
    # The allocator patch lives in the toolchain, which ninja does not watch: a program linked
    # before it (or before a newer version of it) is removed so the link runs again.
    if zigfix.apply():
        wkpins.log("zig: SmpAllocator now has at least %d slots (tools/webkit/zigfix.py)" % zigfix.MIN_SLOTS)
    record = BUILD / ".lazyos-allocator.json"
    old = zigfix.stale(record, targets)
    for name in old:
        (BUILD / "bin" / OUTPUTS.get(name, name)).unlink(missing_ok=True)
    if old:
        wkpins.log("linking again with the fixed allocator: " + ", ".join(old))
    began = time.monotonic()
    run(["ninja", "-C", str(BUILD), f"-j{jobs}", "-k", "0", *targets], BUILD, "webkit port build",
        env={**buildkit.pkg_env(), **LINK_ENV})
    wkpins.log(f"ninja finished in {time.monotonic() - began:.0f} s")
    zigfix.mark(record, targets)
    return collect(targets)


def collect(targets: list[str]) -> dict[str, str]:
    found: dict[str, str] = {}
    wkpins.BIN.mkdir(parents=True, exist_ok=True)
    try:
        records = json.loads(STRIPPED.read_text())
    except (OSError, ValueError):
        records = {}
    for name in targets:
        built = BUILD / "bin" / OUTPUTS.get(name, name)
        if not built.is_file():
            raise BuildError(f"ninja finished but {built} is missing")
        out = wkpins.BIN / name
        # A program ninja did not link again is not stripped again: the image
        # build and the launcher watch these files' times, and a rewritten
        # 100 MiB program sent every later step round again. "Linked again" is
        # the program's own size and time against the record of the last strip
        # (both on the build volume: no clock of another file system is read).
        stat = built.stat()
        signature = [stat.st_size, stat.st_mtime_ns]
        if not (out.is_file() and records.get(name) == signature):
            # Beside it and renamed: a cancelled strip leaves no short program.
            part = out.with_name(out.name + ".part")
            run(["strip", "--strip-all", "-o", str(part), str(built)], BUILD, f"strip {name}")
            part.chmod(0o755)
            os.replace(part, out)
            records[name] = signature
        found[name] = str(out)
    STRIPPED.write_text(json.dumps(records, indent=1))
    write_fonts_conf()
    sizes = {name: Path(path).stat().st_size for name, path in found.items()}
    record = wkpins.OUT / "port.json"
    try:
        previous = json.loads(record.read_text()).get("sizes")
    except (OSError, ValueError):
        previous = None
    if previous != sizes:
        record.write_text(json.dumps({"sizes": sizes, "built_at": time.time()}, indent=1))
    return found


def write_fonts_conf() -> Path:
    """``target/webkit/fonts.conf``: the template with the paths of ``libs/fhs`` (the image embeds it)."""
    wkpins.OUT.mkdir(parents=True, exist_ok=True)
    text = fhspaths.fonts_conf((PORT_DIR / "fonts.conf.in").read_text(encoding="utf-8"))
    # Only a changed file may be written: the image build watches its time.
    if not FONTS_CONF.is_file() or FONTS_CONF.read_text(encoding="utf-8") != text:
        FONTS_CONF.write_text(text, encoding="utf-8")
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
#: For each program stripped into `target/webkit/bin`: the size and time of the linked
#: program it was made from (`collect`).
STRIPPED = wkpins.OUT / "stripped.json"
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
                                  timeout=300, cwd=scratch, env=smoke_environment())
        except subprocess.TimeoutExpired as error:
            raise BuildError("the host reference run of webkit-smoke timed out after 300 s:\n"
                             + _tail(error.stdout)) from error
    if done.returncode != 0 or "WEBKIT:W4:PASS" not in done.stdout:
        raise BuildError(f"the host reference run of webkit-smoke failed (exit {done.returncode}):\n"
                         + done.stdout[-1500:] + done.stderr[-500:])
    REFERENCE.write_text(done.stdout)
    wkpins.log("host reference run passed")
    return True


def run_views() -> bool:
    """Run ``webkit-views`` (``webkit/port/views``, docs/webd-plan.md P1) on the build host: the engine's
    views without a window, once with one light view, three times with two views (of different profiles, of none, of the same one),
    and twice making and destroying twelve views in turn (after they painted, and at once).
    Each run prints ``VIEWS:`` lines (the engine processes it launched among them) and ends with
    ``VIEWS:PASS``; a run that does not is a ``BuildError``. Only in the build container, like
    ``host_reference``; False when it did not run."""
    import subprocess
    import tempfile
    if os.environ.get("LAZYOS_JSC_IN_CONTAINER") != "1":
        wkpins.log("not in the build container: webkit-views did not run")
        return False
    install_host_layout()
    if not (HOST_BIN / "webkit-views").is_file():
        raise BuildError("webkit-views is not built (python tools/webkit/port.py --only webkit-views)")
    wkpins.jsc_pins.WORK.mkdir(parents=True, exist_ok=True)
    for mode in ("light", "twoeph", "two", "share", "group", "groupprof", "groupmix", "cycle", "rush"):
        with tempfile.TemporaryDirectory(prefix="webkit-views-", dir=wkpins.jsc_pins.WORK,
                                         ignore_cleanup_errors=True) as scratch:
            try:
                done = subprocess.run([str(HOST_BIN / "webkit-views"), mode], capture_output=True, text=True,
                                      timeout=300, cwd=scratch, env=smoke_environment())
            except subprocess.TimeoutExpired as error:
                raise BuildError(f"webkit-views {mode} timed out after 300 s:\n" + _tail(error.stdout)) from error
        lines = [line for line in done.stdout.splitlines() if line.startswith("VIEWS:")]
        wkpins.log(f"webkit-views {mode}:\n" + "\n".join(lines))
        if done.returncode != 0 or "VIEWS:PASS" not in done.stdout:
            raise BuildError(f"webkit-views {mode} failed (exit {done.returncode}):\n"
                             + done.stdout[-2500:] + done.stderr[-500:])
    return True


#: The modes of ``webd-abi-test`` (webkit/port/webd/test): each runs in a process of its own. The first eight
#: are light views (P2 slice 1), the rest full views (slice 3).
WEBD_MODES = ("html", "script", "config", "lock", "network", "many", "lost", "cycle",
              "full", "profiles", "ask", "dialog", "download", "popup", "deltas", "blocking", "notify", "nonet")


def run_webd(modes: list[str] | None = None) -> bool:
    """Run ``webd-abi-test`` (``webkit/port/webd``, docs/webd-plan.md P2 slice 1) on the build host: the
    engine's half of webd driven through ``webd_abi.h`` by a program that plays the service, one mode per
    process (all of them, or the `modes` named). Each prints ``WEBD:`` lines and ends with
    ``WEBD:<mode>:PASS``; a mode that does not is a ``BuildError``. Only in the build container, like ``run_views``; False when it did not run."""
    import subprocess
    import tempfile
    if os.environ.get("LAZYOS_JSC_IN_CONTAINER") != "1":
        wkpins.log("not in the build container: webd-abi-test did not run")
        return False
    install_host_layout()
    if not (HOST_BIN / "webd-abi-test").is_file():
        raise BuildError("webd-abi-test is not built (python tools/webkit/port.py --only webd-abi-test)")
    wkpins.jsc_pins.WORK.mkdir(parents=True, exist_ok=True)
    for mode in modes or WEBD_MODES:
        if mode not in WEBD_MODES:
            raise BuildError(f"webd-abi-test has no mode {mode} (modes: {', '.join(WEBD_MODES)})")
        with tempfile.TemporaryDirectory(prefix="webd-abi-", dir=wkpins.jsc_pins.WORK,
                                         ignore_cleanup_errors=True) as scratch:
            try:
                done = subprocess.run([str(HOST_BIN / "webd-abi-test"), mode], capture_output=True, text=True,
                                      timeout=400, cwd=scratch, env=smoke_environment())
            except subprocess.TimeoutExpired as error:
                raise BuildError(f"webd-abi-test {mode} timed out after 400 s:\n" + _tail(error.stdout)) from error
        lines = [line for line in done.stdout.splitlines() if line.startswith("WEBD:")]
        wkpins.log(f"webd-abi-test {mode}:\n" + "\n".join(lines))
        if done.returncode != 0 or f"WEBD:{mode}:PASS" not in done.stdout:
            raise BuildError(f"webd-abi-test {mode} failed (exit {done.returncode}):\n"
                             + done.stdout[-3000:] + done.stderr[-500:])
    return True


def smoke_environment() -> dict[str, str]:
    """The host run's environment: the descriptor mode the guest's run uses too (issue #867)."""
    import portjudge
    return dict(os.environ, WEBKIT_SMOKE_FDS=str(portjudge.HELD_DESCRIPTORS))


def _tail(output: str | bytes | None) -> str:
    if isinstance(output, bytes):
        output = output.decode(errors="replace")
    return (output or "")[-1500:]
