"""ICU 74.2 as static musl libraries with their data compiled in.

LazyOS has no file-backed ``mmap``, so ICU cannot load ``icudt74l.dat`` at
run time: ``--with-data-packaging=static`` makes ``pkgdata`` emit the data as
an assembly object that ``libicudata.a`` carries, and the program finds it
through the ``icudt74_dat`` symbol.

A cross build needs ICU's own tools (``icupkg``, ``genrb``, ``pkgdata``, ...)
to run on the build machine, so ICU is configured and built twice: first for
the host (plain gcc, only for its tools), then for static musl with zig and
``--with-cross-build`` pointing at the first.

The data is cut before it is compiled in (``icudata.py``: 30.8 MB of the
prebuilt ``icudt74l.dat`` become about 9 MB): every process that links ICU
carries it in RAM. A change to that policy (its signature is recorded in
``lib/.icudata``) rebuilds only the data library of an existing build.
"""

from __future__ import annotations

import shutil
from pathlib import Path

import icudata
import pins
import toolenv
from toolenv import BuildError, run

PREFIX = pins.WORK / "prefix"
#: Features nobody here needs; each skipped directory is build time saved.
COMMON = ["--disable-tests", "--disable-samples", "--disable-extras",
          "--disable-layoutex", "--enable-static", "--disable-shared"]
STAMP = PREFIX / "lib" / "libicudata.a"
#: The policy signature (`icudata.signature`) the installed data library was made with.
DATA_MARK = PREFIX / "lib" / ".icudata"


def data_current() -> bool:
    return DATA_MARK.is_file() and DATA_MARK.read_text().strip() == icudata.signature()


def trim_data(tree: Path, host: Path) -> None:
    """Replace the tree's prebuilt data package by the cut one (always cut from the pristine copy)."""
    package = tree / "source" / "data" / "in" / "icudt74l.dat"
    pristine = icudata.keep_copy(package)
    work = pins.WORK / "icu-data"
    work.mkdir(parents=True, exist_ok=True)
    cut = work / "icudt74l.dat"
    try:
        stats = icudata.trim(host / "bin" / "icupkg", pristine, cut, work)
        compared = icudata.check(host, tree, cut, work)
    except (RuntimeError, OSError) as error:
        raise BuildError(f"icu data: {error}") from error
    shutil.copyfile(cut, package)
    pins.log(f"icu data: {stats['before']} -> {stats['after']} bytes "
             f"({stats['dropped']} of {stats['items']} items dropped, {compared} results compared)")


def build(jobs: int) -> Path:
    """Build and install ICU for the target; the install prefix."""
    if STAMP.is_file() and data_current():
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
    trim_data(tree, host)

    cross = pins.WORK / "icu-target"
    cross.mkdir(parents=True, exist_ok=True)
    if not (STAMP.is_file() and (cross / "Makefile").is_file()):
        flags = " ".join(toolenv.CODEGEN)
        defs = "-DU_CHARSET_IS_UTF8=1 -DU_USING_ICU_NAMESPACE=0 -DU_STATIC_IMPLEMENTATION"
        run([str(configure), *COMMON, "--host=x86_64-linux-musl",
             f"--with-cross-build={host}", f"--prefix={PREFIX}",
             "--with-data-packaging=static"], cross,
            "icu target configure",
            env={"CC": wrap["zcc"], "CXX": wrap["zc++"], "AR": wrap["zar"],
                 "RANLIB": wrap["zranlib"], "CFLAGS": flags, "CXXFLAGS": flags + " -std=c++17",
                 "CPPFLAGS": defs, "LDFLAGS": " ".join(toolenv.LINK)})
    else:
        # Only the data changed: make rebuilds the assembly object from the new package.
        shutil.rmtree(cross / "data" / "out" / "tmp", ignore_errors=True)
        (cross / "lib" / "libicudata.a").unlink(missing_ok=True)
    run(["make", f"-j{jobs}"], cross, "icu target make")
    run(["make", "install"], cross, "icu target install")
    DATA_MARK.write_text(icudata.signature() + "\n")
    return PREFIX
