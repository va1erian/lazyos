"""WebKit's JSCOnly port: the ``jsc`` shell as a static musl executable.

JavaScriptCore, WTF and bmalloc come from the WebKitGTK release tarball (the
smallest official download that holds them). CMake is told to build the
``JSCOnly`` port twice (W2 of ``docs/webkit-port-plan.md``): with the C-loop
interpreter (``jsc``) and with the JIT, LLInt in assembly, baseline and DFG
(``jsc-jit``, ``build.py --jit``); WebAssembly off, the system allocator
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

#: The two configurations: the C-loop interpreter (`jsc`) and the JIT (`jsc-jit`,
#: baseline and DFG; `build.py --jit`). Each has a build tree of its own
#: and shares the extracted, patched source tree.
CONFIGS = {
    "jsc": {"dir": "webkit-build", "jit": False},
    "jsc-jit": {"dir": "webkit-build-jit", "jit": True},
}

#: The configuration the brief fixes; the rest are CMake plumbing.
OPTIONS = {
    "PORT": "JSCOnly",
    "ENABLE_STATIC_JSC": "ON",
    "ENABLE_WEBASSEMBLY": "OFF",
    "ENABLE_SAMPLING_PROFILER": "OFF",  # conflicts with the C-loop; unused with the JIT
    "ENABLE_FUZZILLI": "OFF",
    "USE_HEADER_MAPS": "OFF",  # Tools/Scripts/hmaptool is not in the tarball
    "USE_SYSTEM_MALLOC": "ON",
    "DEVELOPER_MODE": "OFF",
    "CMAKE_BUILD_TYPE": "Release",
}


def options(jit: bool) -> dict[str, str]:
    """The CMake options of one configuration (`JIT` on means no C-loop)."""
    if not jit:
        return {**OPTIONS, "ENABLE_JIT": "OFF", "ENABLE_C_LOOP": "ON"}
    # FTL (B3) does not build with WebAssembly off at this revision (B3's Wasm
    # values need the Wasm headers), so the JIT shell has LLInt, baseline and DFG.
    return {**OPTIONS, "ENABLE_JIT": "ON", "ENABLE_C_LOOP": "OFF", "ENABLE_FTL_JIT": "OFF"}


def configure(tree: Path, wrap: dict[str, str], build: Path, jit: bool) -> None:
    build.mkdir(parents=True, exist_ok=True)
    flags = " ".join(toolenv.CODEGEN)
    # simdutf's AVX2/AVX-512/SSE4 paths are runtime-dispatched on CPUID and need
    # clang's `evex512`; LazyOS enables no AVX state (docs/tls-plan.md 8), so only
    # its portable implementation is built.
    simd = " ".join(f"-DSIMDUTF_IMPLEMENTATION_{n}=0" for n in ("ICELAKE", "HASWELL", "WESTMERE"))
    command = ["cmake", "-G", "Ninja", "-S", str(tree), "-B", str(build),
               *[f"-D{key}={value}" for key, value in options(jit).items()],
               f"-DCMAKE_C_COMPILER={wrap['zcc']}", f"-DCMAKE_CXX_COMPILER={wrap['zc++']}",
               f"-DCMAKE_AR={wrap['zar']}", f"-DCMAKE_RANLIB={wrap['zranlib']}",
               f"-DCMAKE_C_FLAGS={flags}", f"-DCMAKE_CXX_FLAGS={flags} {simd} -std=c++2b",
               f"-DCMAKE_EXE_LINKER_FLAGS={' '.join(toolenv.LINK)}",
               f"-DICU_ROOT={icu.PREFIX}", f"-DCMAKE_PREFIX_PATH={icu.PREFIX}"]
    # Configure again only when the command changed (the stamp is the command).
    stamp = build / ".lazyos-configured"
    wanted = " ".join(command)
    if (build / "build.ninja").is_file() and stamp.is_file() and stamp.read_text() == wanted:
        return
    run(command, build, f"webkit configure {build.name}")
    stamp.write_text(wanted)


def build(jobs: int, name: str = "jsc") -> Path:
    """Fetch, patch, configure and build one shell (`jsc` or `jsc-jit`); the stripped executable."""
    config = CONFIGS[name]
    build_dir = pins.WORK / config["dir"]
    archive = pins.fetch(pins.WEBKIT)
    if archive is None:
        raise BuildError("WebKitGTK download unavailable")
    tree = toolenv.extract(archive, pins.WEBKIT, pins.WORK / "webkit-src")
    toolenv.apply_patches(tree, "webkit")
    configure(tree, toolenv.wrappers(), build_dir, config["jit"])
    run(["ninja", "-C", str(build_dir), f"-j{jobs}", "-k", "0", "jsc"], build_dir,
        f"webkit build {name}")
    built = build_dir / "bin" / "jsc"
    if not built.is_file():
        raise BuildError(f"ninja finished but {built} is missing")
    out = pins.OUT / name
    out.parent.mkdir(parents=True, exist_ok=True)
    # Stripped: debugging a guest crash uses addresses, not the host's symbols.
    run(["strip", "--strip-all", "-o", str(out), str(built)], pins.WORK, f"strip {name}")
    out.chmod(0o755)
    return out
