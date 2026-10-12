"""Recipes: text and data (ICU copy, FreeType, expat, HarfBuzz, Fontconfig,
libxml2, SQLite, libpsl)."""

from __future__ import annotations

import shutil
from pathlib import Path

import buildkit
import fhspaths
import toolenv
import wkpins
from buildkit import PREFIX
from registry import Lib, register
from toolenv import run

#: Where the guest looks (baked into Fontconfig), from libs/fhs (fhspaths.py);
#: the image ships the fonts later (W4).
FONT_DIR = fhspaths.FONT_DIR
FONT_CONF_DIR = fhspaths.FONT_CONF_DIR
FONT_CACHE_DIR = fhspaths.FONT_CACHE_DIR

FREETYPE = {"FT_REQUIRE_ZLIB": "ON", "FT_REQUIRE_PNG": "ON", "FT_REQUIRE_BROTLI": "ON",
            "FT_DISABLE_BZIP2": "ON", "FT_DISABLE_HARFBUZZ": "ON",
            "BROTLIDEC_INCLUDE_DIRS": f"{PREFIX}/include",
            "BROTLIDEC_LIBRARIES": f"{PREFIX}/lib/libbrotlidec.a;{PREFIX}/lib/libbrotlicommon.a"}
EXPAT = {"EXPAT_BUILD_TOOLS": "OFF", "EXPAT_BUILD_EXAMPLES": "OFF", "EXPAT_BUILD_TESTS": "OFF",
         "EXPAT_BUILD_DOCS": "OFF", "EXPAT_SHARED_LIBS": "OFF"}
HARFBUZZ = {"HB_HAVE_FREETYPE": "ON", "HB_HAVE_ICU": "ON", "HB_HAVE_GLIB": "OFF",
            "HB_HAVE_GOBJECT": "OFF", "HB_HAVE_GRAPHITE2": "OFF", "HB_BUILD_UTILS": "OFF",
            "HB_BUILD_TESTS": "OFF", "HB_BUILD_SUBSET": "ON"}
FONTCONFIG = ["--enable-static", "--disable-shared", "--disable-docs", "--disable-nls",
              "--disable-cache-build", f"--sysconfdir={FONT_CONF_DIR}", "--localstatedir=/var",
              f"--with-default-fonts={FONT_DIR}", f"--with-cache-dir={FONT_CACHE_DIR}"]
LIBXML2 = {"LIBXML2_WITH_ICONV": "OFF", "LIBXML2_WITH_ICU": "ON", "LIBXML2_WITH_LZMA": "OFF",
           "LIBXML2_WITH_ZLIB": "ON", "LIBXML2_WITH_PYTHON": "OFF", "LIBXML2_WITH_PROGRAMS": "OFF",
           "LIBXML2_WITH_TESTS": "OFF", "LIBXML2_WITH_MODULES": "OFF", "LIBXML2_WITH_THREADS": "ON",
           "LIBXML2_WITH_DOCB": "OFF"}
SQLITE_DEFS = ["-DSQLITE_THREADSAFE=1", "-DSQLITE_DEFAULT_MEMSTATUS=0", "-DSQLITE_ENABLE_FTS3",
               "-DSQLITE_ENABLE_FTS3_PARENTHESIS", "-DSQLITE_ENABLE_COLUMN_METADATA",
               "-DSQLITE_ENABLE_RTREE", "-DSQLITE_OMIT_LOAD_EXTENSION", "-DSQLITE_ENABLE_STAT4"]
LIBPSL = ["--enable-static", "--disable-shared", "--enable-runtime=libicu",
          "--enable-builtin=libicu", "--disable-man", "--disable-gtk-doc"]


ICU = wkpins.BUNDLED_BY_NAME["icu"]


def _cmake(name: str, options: dict):
    def build(tree: Path, jobs: int) -> None:
        buildkit.cmake_build(name, tree, options, jobs)
    return build


def _icu(tree: Path, jobs: int) -> None:
    """Copy the ICU the ``jsc`` build made (tools/jsc/icu.py) into this prefix."""
    import icu
    source = icu.build(jobs)
    into = buildkit.staged_prefix()
    shutil.copytree(source / "include" / "unicode", into / "include" / "unicode", dirs_exist_ok=True)
    (into / "lib" / "pkgconfig").mkdir(parents=True, exist_ok=True)
    for archive in sorted((source / "lib").glob("libicu*.a")):
        shutil.copy2(archive, into / "lib" / archive.name)
    for pc in sorted((source / "lib" / "pkgconfig").glob("icu-*.pc")):
        text = pc.read_text().replace(str(source), str(PREFIX))
        (into / "lib" / "pkgconfig" / pc.name).write_text(text)


def _fontconfig(tree: Path, jobs: int) -> None:
    """Build for the guest's paths, then keep the sample configuration out of /system."""
    destdir = buildkit.stage()
    build = wkpins.BUILD / "fontconfig"
    shutil.rmtree(build, ignore_errors=True)
    build.mkdir(parents=True)
    env = buildkit.tool_env()
    env["CC_FOR_BUILD"] = "gcc"
    run([str(tree / "configure"), f"--prefix={PREFIX}", f"--libdir={PREFIX}/lib", *FONTCONFIG],
        build, "fontconfig configure", env=env)
    run(["make", f"-j{jobs}"], build, "fontconfig make", env=env)
    run(["make", "install", f"DESTDIR={destdir}"], build, "fontconfig install", env=env)
    # The install also wrote the guest's configuration directory under the
    # stage; it belongs to the image (W4), so a copy rides in the prefix's share.
    conf = destdir / FONT_CONF_DIR.lstrip("/") / "fonts"
    if conf.is_dir():
        shutil.copytree(conf, buildkit.staged_prefix() / "share" / "fontconfig-etc" / "fonts",
                        dirs_exist_ok=True)


def _sqlite(tree: Path, jobs: int) -> None:
    wrap = toolenv.wrappers()
    work = wkpins.BUILD / "sqlite"
    shutil.rmtree(work, ignore_errors=True)
    work.mkdir(parents=True)
    run([wrap["zcc"], *toolenv.CODEGEN, *SQLITE_DEFS, "-c", str(tree / "sqlite3.c"), "-o", "sqlite3.o"],
        work, "sqlite compile")
    run([wrap["zar"], "rcs", "libsqlite3.a", "sqlite3.o"], work, "sqlite archive")
    into = buildkit.staged_prefix()
    (into / "lib" / "pkgconfig").mkdir(parents=True, exist_ok=True)
    (into / "include").mkdir(parents=True, exist_ok=True)
    shutil.copy2(work / "libsqlite3.a", into / "lib" / "libsqlite3.a")
    for header in ("sqlite3.h", "sqlite3ext.h"):
        shutil.copy2(tree / header, into / "include" / header)
    version = wkpins.BY_NAME["sqlite"].source.version
    (into / "lib" / "pkgconfig" / "sqlite3.pc").write_text(
        f"prefix={PREFIX}\nlibdir=${{prefix}}/lib\nincludedir=${{prefix}}/include\n\n"
        f"Name: SQLite\nDescription: SQL database engine\nVersion: {version}\n"
        "Libs: -L${libdir} -lsqlite3\nLibs.private: -lpthread -lm\nCflags: -I${includedir}\n")


def _libpsl(tree: Path, jobs: int) -> None:
    # The psl tool links ICU (C++); static libc++ is not in a C link line, so name it.
    buildkit.configure_build("libpsl", tree, LIBPSL, jobs, env={"LIBS": "-lc++"})


register(Lib("icu", [], {"from": "tools/jsc/icu.py"}, _icu, own_tree=True,
             extra={"version": ICU.source.version, "source_sha": ICU.source.sha256,
                    "info": {"url": ICU.source.url, "license": ICU.license, "why": ICU.why}}))
register(Lib("freetype", ["zlib", "libpng", "brotli"], FREETYPE, _cmake("freetype", FREETYPE)))
register(Lib("expat", [], EXPAT, _cmake("expat", EXPAT)))
register(Lib("harfbuzz", ["icu", "freetype"], HARFBUZZ, _cmake("harfbuzz", HARFBUZZ)))
register(Lib("fontconfig", ["freetype", "expat"], FONTCONFIG, _fontconfig))
register(Lib("libxml2", ["zlib", "icu"], LIBXML2, _cmake("libxml2", LIBXML2)))
register(Lib("sqlite", [], SQLITE_DEFS, _sqlite))
register(Lib("libpsl", ["icu"], LIBPSL, _libpsl))
