"""Recipes for the WebKit port (W4): the EGL/GLESv2 stand-in and libwpe.

* ``glstub``: ``libEGL.a`` and ``libGLESv2.a`` in which every function does
  nothing (``webkit/port/glstub/gen_stub.py`` writes them from the Khronos
  headers WebKit ships in ``ThirdParty/ANGLE/include``, which are installed
  too). LazyOS has no GPU; the code that would call them is compiled and never
  reaches them.
* ``libwpe``: the PlayStation port's WebKit layer takes its input-event,
  pasteboard and key-code types from libwpe (a few thousand lines of C, built
  here without libxkbcommon and without a backend: WebKit's view backend and
  renderer are not used).
"""

from __future__ import annotations

import shutil
import sys
from pathlib import Path

import buildkit
import skia
import toolenv
import wkpins
from buildkit import PREFIX
from registry import Lib, register
from toolenv import run

GLSTUB = wkpins.ROOT / "webkit" / "port" / "glstub"
#: ANGLE's copy of the Khronos headers, installed so `find_package(EGL)` finds them.
HEADER_DIRS = ["EGL", "GLES2", "GLES3", "KHR"]
PKGCONFIG = """prefix={prefix}
libdir=${{prefix}}/lib
includedir=${{prefix}}/include

Name: {name}
Description: {description}
Version: 1.5
Libs: -L${{libdir}} -l{lib}
Cflags: -I${{includedir}}
"""
LIBWPE = {"BUILD_SHARED_LIBS": "OFF", "WPE_ENABLE_XKB": "OFF", "BUILD_DOCS": "OFF"}


def _digest() -> str:
    import hashlib
    return hashlib.sha256((GLSTUB / "gen_stub.py").read_bytes()).hexdigest()[:12]


def _glstub(_tree: Path, jobs: int) -> None:
    angle = skia.webkit_tree() / "Source" / "ThirdParty" / "ANGLE" / "include"
    work = wkpins.BUILD / "glstub"
    shutil.rmtree(work, ignore_errors=True)
    work.mkdir(parents=True)
    run([sys.executable, str(GLSTUB / "gen_stub.py"), str(angle), str(work)], work, "glstub generate")
    wrap = toolenv.wrappers()
    for name, lib in (("egl_stub", "libEGL.a"), ("gles_stub", "libGLESv2.a")):
        run([wrap["zcc"], *toolenv.CODEGEN, f"-I{angle}", "-c", str(work / f"{name}.c"), "-o", str(work / f"{name}.o")],
            work, f"glstub compile {name}")
        run([wrap["zar"], "rcs", str(PREFIX / "lib" / lib), str(work / f"{name}.o")], work, f"glstub archive {lib}")
    for directory in HEADER_DIRS:
        shutil.copytree(angle / directory, PREFIX / "include" / directory, dirs_exist_ok=True)
    pc = PREFIX / "lib" / "pkgconfig"
    pc.mkdir(parents=True, exist_ok=True)
    (pc / "egl.pc").write_text(PKGCONFIG.format(prefix=PREFIX, name="egl", lib="EGL",
                                                  description="EGL stand-in (no display)"))
    (pc / "glesv2.pc").write_text(PKGCONFIG.format(prefix=PREFIX, name="glesv2", lib="GLESv2",
                                                     description="OpenGL ES 2 stand-in (no GPU)"))


def _libwpe(tree: Path, jobs: int) -> None:
    buildkit.cmake_build("libwpe", tree, LIBWPE, jobs)
    # libwpe names only its shared library `libwpe-1.0`, but wpe-1.0.pc asks for -lwpe-1.0.
    shutil.copy(PREFIX / "lib" / "libwpe.a", PREFIX / "lib" / "libwpe-1.0.a")


register(Lib("glstub", [], {"gen": _digest()}, _glstub, own_tree=True,
             extra={"version": "Khronos headers of WebKitGTK 2.54.1 ThirdParty/ANGLE",
                    "source_sha": wkpins.jsc_pins.WEBKIT.sha256,
                    "info": {"license": "MIT", "why": "link-time stand-in for EGL and GLESv2"}}))
register(Lib("libwpe", ["glstub"], LIBWPE, _libwpe))
