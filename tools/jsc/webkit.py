"""WebKit's JSCOnly port: the ``jsc`` shell as a static musl executable.

JavaScriptCore, WTF and bmalloc come from the WebKitGTK release tarball (the
smallest official download that holds them). CMake is told to build the
``JSCOnly`` port with the C-loop interpreter (no JIT yet: W2 of
``docs/webkit-port-plan.md``), WebAssembly off, the system allocator
(``USE_SYSTEM_MALLOC``, so bmalloc's own is not linked in) and ICU from
``icu.py``. Every change to the tarball is a file in ``patches/webkit/``,
applied once by name order; nothing is edited by hand.

The generators WebKit runs while building (offset extractors, the
``LLInt`` settings, derived sources) are static musl x86-64 programs, which a
Linux x86-64 host runs as they are, so no host/target split is needed.
"""

from __future__ import annotations

import shutil
from pathlib import Path

import icu
import pins
import toolenv
from toolenv import BuildError, run

BUILD = pins.WORK / "webkit-build"

#: The configuration the brief fixes; the rest are CMake plumbing.
OPTIONS = {
    "PORT": "JSCOnly",
    "ENABLE_STATIC_JSC": "ON",
    "ENABLE_JIT": "OFF",
    "ENABLE_C_LOOP": "ON",
    "ENABLE_WEBASSEMBLY": "OFF",
    "ENABLE_SAMPLING_PROFILER": "OFF",  # conflicts with the C-loop
    "ENABLE_FUZZILLI": "OFF",
    "USE_HEADER_MAPS": "OFF",  # Tools/Scripts/hmaptool is not in the tarball
    "USE_SYSTEM_MALLOC": "ON",
    "DEVELOPER_MODE": "OFF",
    "CMAKE_BUILD_TYPE": "Release",
}


def configure(tree: Path, wrap: dict[str, str]) -> None:
    BUILD.mkdir(parents=True, exist_ok=True)
    flags = " ".join(toolenv.CODEGEN)
    # simdutf's AVX2/AVX-512/SSE4 paths are runtime-dispatched on CPUID and need
    # clang's `evex512`; LazyOS enables no AVX state (docs/tls-plan.md 8), so only
    # its portable implementation is built.
    simd = " ".join(f"-DSIMDUTF_IMPLEMENTATION_{n}=0" for n in ("ICELAKE", "HASWELL", "WESTMERE"))
    command = ["cmake", "-G", "Ninja", "-S", str(tree), "-B", str(BUILD),
               *[f"-D{key}={value}" for key, value in OPTIONS.items()],
               f"-DCMAKE_C_COMPILER={wrap['zcc']}", f"-DCMAKE_CXX_COMPILER={wrap['zc++']}",
               f"-DCMAKE_AR={wrap['zar']}", f"-DCMAKE_RANLIB={wrap['zranlib']}",
               f"-DCMAKE_C_FLAGS={flags}", f"-DCMAKE_CXX_FLAGS={flags} {simd} -std=c++2b",
               f"-DCMAKE_EXE_LINKER_FLAGS={' '.join(toolenv.LINK)}",
               f"-DICU_ROOT={icu.PREFIX}", f"-DCMAKE_PREFIX_PATH={icu.PREFIX}"]
    # Configure again only when the command changed (the stamp is the command).
    stamp = BUILD / ".lazyos-configured"
    wanted = " ".join(command)
    if (BUILD / "build.ninja").is_file() and stamp.is_file() and stamp.read_text() == wanted:
        return
    run(command, BUILD, "webkit configure")
    stamp.write_text(wanted)


def build(jobs: int) -> Path:
    """Fetch, patch, configure and build `jsc`; the stripped executable."""
    archive = pins.fetch(pins.WEBKIT)
    if archive is None:
        raise BuildError("WebKitGTK download unavailable")
    tree = toolenv.extract(archive, pins.WEBKIT, pins.WORK / "webkit-src")
    toolenv.apply_patches(tree, "webkit")
    configure(tree, toolenv.wrappers())
    run(["ninja", "-C", str(BUILD), f"-j{jobs}", "-k", "0", "jsc"], BUILD, "webkit build")
    built = BUILD / "bin" / "jsc"
    if not built.is_file():
        raise BuildError(f"ninja finished but {built} is missing")
    out = pins.OUT / "jsc"
    out.parent.mkdir(parents=True, exist_ok=True)
    # Stripped: debugging a guest crash uses addresses, not the host's symbols.
    run(["strip", "--strip-all", "-o", str(out), str(built)], pins.WORK, "strip jsc")
    out.chmod(0o755)
    return out
