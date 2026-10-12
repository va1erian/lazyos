"""``depsproof``: one C++ program that links every library of the bundle and
uses each (``depsproof/*.cpp``), built as a static-PIE musl executable.

The embedded font (``fontdata.cpp``) is generated here from the Liberation Sans
the image ships (``assets/fonts/liberation``, SIL OFL 1.1), because a console
image has no fonts. A static musl program also runs on the Linux build host, so
the build runs ``depsproof`` once offline there and keeps its output as
``target/webkit/depsproof.host.txt``: the pixels and digests the guest must
reproduce.
"""

from __future__ import annotations

import subprocess
from pathlib import Path

import toolenv
import wkpins
from toolenv import BuildError, run

SOURCES = sorted((wkpins.HERE / "depsproof").glob("*.cpp"))
PREFIX = wkpins.PREFIX
FONT = wkpins.ROOT / "assets" / "fonts" / "liberation" / "LiberationSans-Regular.ttf"
HOST_RUN = wkpins.OUT / "depsproof.host.txt"
#: The pkg-config modules the program links (the static closure comes from them).
MODULES = ["libcurl", "libnghttp2", "freetype2", "harfbuzz", "harfbuzz-icu", "fontconfig", "expat",
           "libxml-2.0", "libwebp", "libwebpdemux", "libpng", "libjpeg", "libpsl", "sqlite3", "icu-uc",
           "icu-i18n", "openssl", "zlib", "libbrotlidec", "libbrotlienc"]


def pkg_config(*args: str) -> list[str]:
    pkg = f"{PREFIX}/lib/pkgconfig"
    env = {"PKG_CONFIG_PATH": pkg, "PKG_CONFIG_LIBDIR": pkg, "PATH": "/usr/bin:/bin"}
    done = subprocess.run(["pkg-config", "--static", *args, *MODULES], env=env,
                          capture_output=True, text=True)
    if done.returncode != 0:
        raise BuildError(f"pkg-config {' '.join(args)} failed: {done.stderr}")
    return done.stdout.split()


def skia_flags() -> list[str]:
    path = PREFIX / "skia" / "skia-flags.txt"
    return [line for line in path.read_text().splitlines() if line]


def font_source(work: Path) -> Path:
    """``fontdata.cpp``: the font as a byte array."""
    data = FONT.read_bytes()
    lines = ["namespace proof {", "extern const unsigned char kFont[];", "extern const unsigned long kFontSize;",
             "const unsigned char kFont[] = {"]
    for start in range(0, len(data), 24):
        lines.append(",".join(str(b) for b in data[start:start + 24]) + ",")
    lines += ["};", f"const unsigned long kFontSize = {len(data)}UL;", "}"]
    path = work / "fontdata.cpp"
    path.write_text("\n".join(lines) + "\n")
    return path


def build(jobs: int) -> Path:
    """Compile and link ``depsproof``; run it once on the host; the executable."""
    wrap = toolenv.wrappers()
    out = wkpins.BIN / "depsproof"
    out.parent.mkdir(parents=True, exist_ok=True)
    work = wkpins.BUILD / "depsproof"
    work.mkdir(parents=True, exist_ok=True)
    cxx = [wrap["zc++"], *toolenv.CODEGEN, "-std=c++20", f"-I{PREFIX}/include", f"-I{PREFIX}/skia",
           *skia_flags(), *pkg_config("--cflags")]
    objects: list[str] = []
    for source in [*SOURCES, font_source(work)]:
        obj = work / (source.stem + ".o")
        run([*cxx, "-I" + str(wkpins.HERE / "depsproof"), "-c", str(source), "-o", str(obj)], work,
            f"depsproof compile {source.name}")
        objects.append(str(obj))
    link = [wrap["zc++"], *toolenv.CODEGEN, *objects, f"{PREFIX}/lib/libSkia.a", *pkg_config("--libs"),
            *toolenv.LINK, "-s", "-o", str(out)]
    run(link, work, "depsproof link")
    out.chmod(0o755)
    reference(out, work)
    return out


def reference(program: Path, work: Path) -> None:
    """Run the offline checks on the build host and keep what it printed."""
    done = subprocess.run([str(program)], cwd=work, capture_output=True, text=True, timeout=300)
    HOST_RUN.write_text(done.stdout)
    if done.returncode != 0 or "WEBKIT:W3:OFFLINE" not in done.stdout:
        raise BuildError(f"depsproof failed on the build host (exit {done.returncode}):\n{done.stdout}{done.stderr}")
