"""ICU 74.2 as static musl libraries with their data compiled in.

LazyOS has no file-backed ``mmap``, so ICU cannot load ``icudt74l.dat`` at
run time: ``--with-data-packaging=static`` makes ``pkgdata`` emit the data as
an assembly object that ``libicudata.a`` carries, and the program finds it
through the ``icudt74_dat`` symbol.

A cross build needs ICU's own tools (``icupkg``, ``genrb``, ``pkgdata``, ...)
to run on the build machine, so ICU is configured and built twice: first for
the host (plain gcc, only for its tools), then for static musl with zig and
``--with-cross-build`` pointing at the first.
"""

from __future__ import annotations

from pathlib import Path

import pins
import toolenv
from toolenv import BuildError, run

PREFIX = pins.WORK / "prefix"
#: Features nobody here needs; each skipped directory is build time saved.
COMMON = ["--disable-tests", "--disable-samples", "--disable-extras",
          "--disable-layoutex", "--enable-static", "--disable-shared"]
STAMP = PREFIX / "lib" / "libicudata.a"


def build(jobs: int) -> Path:
    """Build and install ICU for the target; the install prefix."""
    if STAMP.is_file():
        return PREFIX
    archive = pins.fetch(pins.ICU)
    if archive is None:
        raise BuildError("ICU download unavailable")
    tree = toolenv.extract(archive, pins.ICU, pins.WORK / "icu-src")
    toolenv.apply_patches(tree, "icu")
    configure = tree / "source" / "configure"
    wrap = toolenv.wrappers()

    host = pins.WORK / "icu-host"
    host.mkdir(parents=True, exist_ok=True)
    if not (host / "bin" / "pkgdata").is_file():
        run([str(configure), *COMMON], host, "icu host configure",
            env={"CC": "gcc", "CXX": "g++"})
        run(["make", f"-j{jobs}"], host, "icu host make")

    cross = pins.WORK / "icu-target"
    cross.mkdir(parents=True, exist_ok=True)
    flags = " ".join(toolenv.CODEGEN)
    defs = "-DU_CHARSET_IS_UTF8=1 -DU_USING_ICU_NAMESPACE=0 -DU_STATIC_IMPLEMENTATION"
    run([str(configure), *COMMON, "--host=x86_64-linux-musl",
         f"--with-cross-build={host}", f"--prefix={PREFIX}",
         "--with-data-packaging=static"], cross,
        "icu target configure",
        env={"CC": wrap["zcc"], "CXX": wrap["zc++"], "AR": wrap["zar"],
             "RANLIB": wrap["zranlib"], "CFLAGS": flags, "CXXFLAGS": flags + " -std=c++17",
             "CPPFLAGS": defs, "LDFLAGS": " ".join(toolenv.LINK)})
    run(["make", f"-j{jobs}"], cross, "icu target make")
    run(["make", "install"], cross, "icu target install")
    return PREFIX
