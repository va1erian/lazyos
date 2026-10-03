"""Build recipes for the small C programs: Lua, the SQLite shell and jq.

Each recipe takes a :class:`toolchain.Zig` and the output path, works on a
fresh copy of the pinned source (``sources.work_copy``) and compiles an
explicit list of upstream files, unmodified. Nothing runs ``configure``: the
choices it would make for musl are written out here instead.
"""

from __future__ import annotations

from pathlib import Path

import sources
from toolchain import BuildError, Zig

# --- Lua -------------------------------------------------------------------

#: `src/Makefile`'s CORE_O + LIB_O + the interpreter (`lua.c`); not `luac.c`.
LUA_SOURCES = [
    "lapi.c", "lcode.c", "lctype.c", "ldebug.c", "ldo.c", "ldump.c", "lfunc.c",
    "lgc.c", "llex.c", "lmem.c", "lobject.c", "lopcodes.c", "lparser.c",
    "lstate.c", "lstring.c", "ltable.c", "ltm.c", "lundump.c", "lvm.c", "lzio.c",
    "lauxlib.c", "lbaselib.c", "lcorolib.c", "ldblib.c", "liolib.c",
    "lmathlib.c", "loadlib.c", "loslib.c", "lstrlib.c", "ltablib.c",
    "lutf8lib.c", "linit.c",
    "lua.c",
]
#: `LUA_USE_LINUX` is `LUA_USE_POSIX` + `LUA_USE_DLOPEN`. A static binary has
#: no dynamic loader, so only the POSIX half applies (`require` of a C module
#: then says "dynamic libraries not enabled"); there is no readline either.
LUA_FLAGS = ["-std=gnu99", "-DLUA_USE_POSIX"]


def build_lua(cc: Zig, out: Path) -> Path | None:
    tree = sources.work_copy("lua")
    if tree is None:
        return None
    return cc.program(LUA_SOURCES, out, tree / "src", LUA_FLAGS)


# --- SQLite ----------------------------------------------------------------

SQLITE_SOURCES = ["shell.c", "sqlite3.c"]
#: Single-threaded, no loadable extensions (no dlopen in a static binary),
#: no readline: the shell falls back to its own line reader.
SQLITE_FLAGS = ["-DSQLITE_THREADSAFE=0", "-DSQLITE_OMIT_LOAD_EXTENSION"]


def build_sqlite3(cc: Zig, out: Path) -> Path | None:
    tree = sources.work_copy("sqlite3")
    if tree is None:
        return None
    return cc.program(SQLITE_SOURCES, out, tree, SQLITE_FLAGS)


# --- jq --------------------------------------------------------------------

#: `Makefile.am`'s LIBJQ_SRC plus `jq_SOURCES`, in that order.
JQ_SOURCES = [
    "src/builtin.c", "src/bytecode.c", "src/compile.c", "src/execute.c",
    "src/jq_test.c", "src/jv.c", "src/jv_alloc.c", "src/jv_aux.c",
    "src/jv_dtoa.c", "src/jv_file.c", "src/jv_parse.c", "src/jv_print.c",
    "src/jv_unicode.c", "src/linker.c", "src/locfile.c", "src/util.c",
    "src/decNumber/decContext.c", "src/decNumber/decNumber.c",
    "src/jv_dtoa_tsd.c",
    # The release tarball ships the bison/flex output, so no generator runs.
    "src/lexer.c", "src/parser.c",
    "src/main.c",
]

#: What `configure` finds in musl, as `-D` flags (jq has no config.h: its
#: configure passes DEFS on the command line). No oniguruma, so `test`,
#: `match` and friends report that jq was built without regex support.
JQ_FEATURES = [
    "_GNU_SOURCE",          # AC_USE_SYSTEM_EXTENSIONS
    "IEEE_8087",            # little-endian doubles, for jv_dtoa.c
    "USE_DECNUM",           # the bundled decNumber: exact big number literals
    "HAVE_PTHREAD", "HAVE_PTHREAD_KEY_CREATE", "HAVE_PTHREAD_ONCE", "HAVE_ATEXIT",
    "HAVE___THREAD", "HAVE_ALLOCA", "HAVE_ALLOCA_H", "HAVE_MEMMEM",
    "HAVE_ISATTY", "HAVE_STRPTIME", "HAVE_STRFTIME", "HAVE_SETENV",
    "HAVE_TIMEGM", "HAVE_GMTIME_R", "HAVE_GMTIME", "HAVE_LOCALTIME_R",
    "HAVE_LOCALTIME", "HAVE_GETTIMEOFDAY", "HAVE_TM_TM_GMT_OFF", "HAVE_SETLOCALE",
]

#: configure.ac's AC_CHECK_MATH_FUNC list, minus what musl does not have
#: (`__exp10` is macOS's spelling; musl has `tgamma`, not `gamma`). jq builds
#: the missing ones as "Error: <name>/0 not found at build time".
JQ_MATH = [
    "acos", "acosh", "asin", "asinh", "atan2", "atan", "atanh", "cbrt", "ceil",
    "copysign", "cos", "cosh", "drem", "erf", "erfc", "exp10", "exp2", "exp",
    "expm1", "fabs", "fdim", "floor", "fma", "fmax", "fmin", "fmod", "frexp",
    "hypot", "j0", "j1", "jn", "ldexp", "lgamma", "log10", "log1p",
    "log2", "log", "logb", "modf", "lgamma_r", "nearbyint", "nextafter",
    "nexttoward", "pow10", "pow", "remainder", "rint", "round", "scalb", "scalbln",
    "significand", "scalbn", "ilogb", "sin", "sinh", "sqrt", "tan", "tanh",
    "tgamma", "trunc", "y0", "y1", "yn",
]

#: What `src/config_opts.inc` holds when there is no config.status.
JQ_CONFIG = "(unknown)"


def c_string_lines(text: str, newline: bool) -> str:
    """Each line of `text` as a C string literal, one per output line.

    This is jq's Makefile `sed -e 's/\\\\/\\\\\\\\/g' -e 's/"/\\\\"/g' -e 's/^/"/'
    -e 's/$/\\\\n"/'` (``newline=True``, for builtin.inc) and the same without
    the ``\\n`` (for config_opts.inc), done in Python so no sed is needed.
    """
    lines = text.split("\n")
    if lines and lines[-1] == "":
        lines.pop()  # sed sees no line after the final newline
    suffix = "\\n" if newline else ""
    out = []
    for line in lines:
        escaped = line.replace("\\", "\\\\").replace('"', '\\"')
        out.append(f'"{escaped}{suffix}"\n')
    return "".join(out)


def jq_flags() -> list[str]:
    defines = JQ_FEATURES + [f"HAVE_{name.upper()}" for name in JQ_MATH]
    return ["-std=gnu99", "-I.", "-Isrc", *(f"-D{name}=1" for name in defines)]


def write_jq_generated(tree: Path) -> None:
    """The two files `make` generates with sed (version.h ships in the tarball)."""
    builtin = (tree / "src" / "builtin.jq").read_text(encoding="utf-8")
    (tree / "src" / "builtin.inc").write_text(c_string_lines(builtin, True),
                                             encoding="utf-8", newline="\n")
    config = "#define JQ_CONFIG " + c_string_lines(JQ_CONFIG + "\n", False)
    (tree / "src" / "config_opts.inc").write_text(config, encoding="utf-8", newline="\n")
    if not (tree / "src" / "version.h").is_file():
        raise BuildError("jq: the release tarball has no src/version.h")


def build_jq(cc: Zig, out: Path) -> Path | None:
    tree = sources.work_copy("jq")
    if tree is None:
        return None
    write_jq_generated(tree)
    return cc.program(JQ_SOURCES, out, tree, jq_flags())
