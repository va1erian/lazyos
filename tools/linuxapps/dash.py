"""Build dash 0.5.12 as a static musl program, without configure or make.

The steps are `src/Makefile.am`'s, in order:

1. ``config.h`` (hand-written, ``dashgen.CONFIG_H``);
2. ``token.h``/``token_vars.h`` (``mktokens``, in Python);
3. ``builtins.def`` = ``builtins.def.in`` through the target's preprocessor
   with ``config.h`` (zig), then ``builtins.c``/``.h`` (``mkbuiltins``, in Python);
4. ``mkinit``, ``mknodes`` and ``mksyntax`` compiled for the host with zig
   and run unchanged: ``init.c``, ``nodes.c``/``.h``, ``syntax.c``/``.h``;
5. ``signames.c`` (``mksignames``, in Python with the target's numbers);
6. the shell itself: ``dash_CFILES`` plus the generated files.
"""

from __future__ import annotations

from pathlib import Path

import dashgen
import sources
from toolchain import Zig, run

#: `Makefile.am`'s dash_CFILES (the order mkinit sees them in matters for init.c).
DASH_CFILES = [
    "alias.c", "arith_yacc.c", "arith_yylex.c", "cd.c", "error.c", "eval.c",
    "exec.c", "expand.c", "histedit.c", "input.c", "jobs.c", "mail.c", "main.c",
    "memalloc.c", "miscbltin.c", "mystring.c", "options.c", "parser.c",
    "redir.c", "show.c", "trap.c", "output.c", "bltin/printf.c", "system.c",
    "bltin/test.c", "bltin/times.c", "var.c",
]
#: dash_LDADD, as sources.
DASH_GENERATED = ["builtins.c", "init.c", "nodes.c", "signames.c", "syntax.c"]
#: COMMON_CPPFLAGS + DEFAULT_INCLUDES; the target also gets `-include config.h`.
COMMON_FLAGS = ["-DBSD=1", "-DSHELL", "-I.", "-I.."]
TARGET_FLAGS = ["-include", "../config.h", *COMMON_FLAGS]


def _write(path: Path, text: str) -> None:
    path.write_text(text, encoding="utf-8", newline="\n")


def generate(cc: Zig, tree: Path) -> None:
    """Every file `make` generates, written into the copy at `tree`."""
    src = tree / "src"
    _write(tree / "config.h", dashgen.CONFIG_H)
    rows = dashgen.token_table((src / "mktokens").read_text(encoding="utf-8"))
    _write(src / "token.h", dashgen.token_h(rows))
    _write(src / "token_vars.h", dashgen.token_vars_h(rows))
    builtins_def = cc.preprocess(src / "builtins.def.in", src, ["-include", "../config.h"])
    _write(src / "builtins.def", builtins_def)
    _write(src / "builtins.c", dashgen.builtins_c(builtins_def))
    _write(src / "builtins.h", dashgen.builtins_h(builtins_def))
    for tool, args in (("mkinit", DASH_CFILES),
                       ("mknodes", ["nodetypes", "nodes.c.pat"]),
                       ("mksyntax", [])):
        exe = cc.host_program(f"{tool}.c", src, COMMON_FLAGS)
        run([str(exe), *args], src, tool)
    _write(src / "signames.c", dashgen.signames_c())


def build_dash(cc: Zig, out: Path) -> Path | None:
    tree = sources.work_copy("dash")
    if tree is None:
        return None
    generate(cc, tree)
    return cc.program(DASH_CFILES + DASH_GENERATED, out, tree / "src", TARGET_FLAGS)
