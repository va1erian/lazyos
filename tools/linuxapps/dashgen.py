"""dash's generated sources that `make` would make with sh/awk/sed, in Python.

dash 0.5.12 generates part of its source at build time. The three C
generators (``mkinit``, ``mknodes``, ``mksyntax``) are compiled for the host
and run unchanged (``dash.py``). The rest is reimplemented here, because it
either needs a POSIX userland (``mktokens`` and ``mkbuiltins`` are sh scripts
built on awk, sed, sort, nl and tr, with ``/tmp`` files: fragile on a Windows
host) or describes the *target*, not the host (``mksignames`` prints the
build machine's ``<signal.h>``; built on Windows it would give Windows signal
numbers). Each function produces the same text as the tool it replaces;
``test_build.py`` pins the details.

``config.h`` is hand-written too: it is what dash's ``configure`` finds for a
static musl build without libedit.
"""

from __future__ import annotations

import re

# --- config.h ----------------------------------------------------------------

#: dash's `configure` results for x86_64 musl, `--enable-static`, no libedit.
CONFIG_H = """\
/* config.h for dash 0.5.12 on x86_64-linux-musl, written by hand for
 * LazyOS (tools/linuxapps/dashgen.py) instead of running configure. */
#define HAVE_ALIAS_ATTRIBUTE 1
#define HAVE_ALLOCA_H 1
#define HAVE_BSEARCH 1
#define HAVE_DECL_ISBLANK 1
#define HAVE_FACCESSAT 1
#define HAVE_FNMATCH 1
#define HAVE_GETPWNAM 1
#define HAVE_GETRLIMIT 1
#define HAVE_INTTYPES_H 1
#define HAVE_ISALPHA 1
#define HAVE_KILLPG 1
#define HAVE_MEMORY_H 1
#define HAVE_MEMPCPY 1
#define HAVE_PATHS_H 1
#define HAVE_STDINT_H 1
#define HAVE_STDLIB_H 1
#define HAVE_STPCPY 1
#define HAVE_STRCHRNUL 1
#define HAVE_STRINGS_H 1
#define HAVE_STRING_H 1
#define HAVE_STRSIGNAL 1
#define HAVE_STRTOD 1
#define HAVE_STRTOIMAX 1
#define HAVE_STRTOUMAX 1
#define HAVE_ST_MTIM 1
#define HAVE_SYSCONF 1
#define HAVE_SYS_STAT_H 1
#define HAVE_SYS_TYPES_H 1
#define HAVE_UNISTD_H 1
#define PACKAGE "dash"
#define PACKAGE_BUGREPORT ""
#define PACKAGE_NAME "dash"
#define PACKAGE_STRING "dash 0.5.12"
#define PACKAGE_TARNAME "dash"
#define PACKAGE_URL ""
#define PACKAGE_VERSION "0.5.12"
#define VERSION "0.5.12"
#define SIZEOF_INTMAX_T 8
#define SIZEOF_LONG_LONG_INT 8
/* No libedit: no line editing and no `fc` builtin. */
#define SMALL 1
#define WITH_LINENO 1
#define STDC_HEADERS 1
#ifndef _GNU_SOURCE
# define _GNU_SOURCE 1
#endif
/* musl (1.2.4+) has no *64 interfaces; on x86-64 they are the plain ones. */
#define fstat64 fstat
#define lstat64 lstat
#define stat64 stat
#define glob64_t glob_t
#define glob64 glob
#define globfree64 globfree
#define open64 open
#define readdir64 readdir
#define dirent64 dirent
"""

# --- mktokens ----------------------------------------------------------------


def token_table(mktokens: str) -> list[tuple[str, str, str]]:
    """The (name, ends-a-list, label) rows of the heredoc inside `mktokens`.

    The script itself stays the source of truth: its table is read, not copied.
    """
    match = re.search(r"<<\\!\n(.*?)\n!\n", mktokens, re.S)
    if match is None:
        raise ValueError("mktokens: no token table (<<\\! ... !)")
    rows = []
    for line in match.group(1).split("\n"):
        name, endlist, label = re.match(r"(\S+)\s+(\S+)\s+(.*)", line).groups()
        rows.append((name, endlist, label))
    return rows


def token_h(rows: list[tuple[str, str, str]]) -> str:
    return "".join(f"#define {name} {index}\n" for index, (name, _, _) in enumerate(rows))


def token_vars_h(rows: list[tuple[str, str, str]]) -> str:
    out = ["\n/* Array indicating which tokens mark the end of a list */\n",
           "static const char tokendlist[] = {\n"]
    out += [f"\t{endlist},\n" for _, endlist, _ in rows]
    out.append("};\n\nstatic const char *const tokname[] = {\n")
    out += ['\t"' + label.replace('"', '\\"') + '",\n' for _, _, label in rows]
    out.append("};\n\n")
    first = next(i for i, (name, _, _) in enumerate(rows) if name == "TNOT")
    keywords = [label.replace('"', "").split()[0] for _, _, label in rows[first:]]
    out.append(f"#define KWDOFFSET {first}\n\nstatic const char *const parsekwd[] = {{\n")
    out += [f'\t"{word}",\n' for word in keywords[:-1]]
    out.append(f'\t"{keywords[-1]}"\n}};\n')
    return "".join(out)


# --- mkbuiltins --------------------------------------------------------------


def builtin_entries(builtins_def: str) -> tuple[list[str], list[list[str]]]:
    """The C function names, and the sorted [name, (flags,) function] rows."""
    lines = [line.split() for line in builtins_def.split("\n")
             if line.strip() and not line.startswith("#")]
    functions = [fields[0] for fields in lines]
    rows = []
    for fields in lines:
        i = 1
        while i < len(fields):
            if fields[i].startswith("-"):
                rows.append([fields[i + 1], fields[i], fields[0]])
                i += 2
            else:
                rows.append([fields[i], fields[0]])
                i += 1
    # `LC_COLLATE=C sort -k 1,1`: by name, ties by the whole line, bytewise.
    rows.sort(key=lambda row: (row[0].encode(), "\t".join(row).encode()))
    return functions, rows


def _mask(flags: str) -> int:
    mask = 0
    if "s" in flags:
        mask += 1
    if "s" in flags or "u" in flags:
        mask += 2
    if "a" in flags:
        mask += 4
    return mask


def builtins_c(builtins_def: str) -> str:
    functions, rows = builtin_entries(builtins_def)
    out = ["/*\n * This file was generated by the mkbuiltins program.\n */\n\n",
           '#include "shell.h"\n#include "builtins.h"\n\n']
    out += [f"int {function}(int, char **);\n" for function in functions]
    out.append("\nconst struct builtincmd builtincmd[] = {\n")
    for row in rows:
        flags = row[1][1:] if len(row) > 2 else ""
        function = "NULL" if "n" in flags else row[-1]
        out.append(f'\t{{ "{row[0]}", {function}, {_mask(flags)} }},\n')
    out.append("};\n")
    return "".join(out)


BUILTINS_H_TAIL = """
#define BUILTIN_SPECIAL 0x1
#define BUILTIN_REGULAR 0x2
#define BUILTIN_ASSIGN 0x4

struct builtincmd {
\tconst char *name;
\tint (*builtin)(int, char **);
\tunsigned flags;
};

extern const struct builtincmd builtincmd[];
"""


def builtins_h(builtins_def: str) -> str:
    _, rows = builtin_entries(builtins_def)
    # `nl -v0 | sort -u -k 3,3`: each function's first index, by function name.
    first: dict[str, int] = {}
    for index, row in enumerate(rows):
        first.setdefault(row[-1], index)
    out = ["/*\n * This file was generated by the mkbuiltins program.\n */\n\n"]
    for function in sorted(first, key=str.encode):
        out.append(f"#define {function.upper()} (builtincmd + {first[function]})\n")
    out.append(f"\n#define NUMBUILTINS {len(rows)}\n")
    out.append(BUILTINS_H_TAIL)
    return "".join(out)


# --- mksignames --------------------------------------------------------------

#: x86_64 Linux signal numbers (musl's arch/x86_64/bits/signal.h), already
#: resolved the way mksignames resolves aliases: it assigns SIGIOT before
#: SIGABRT, SIGCLD before SIGCHLD and SIGPOLL before SIGIO, so the later
#: names win. SIGSTKFLT (16) is not in its list and prints as "16".
LINUX_SIGNALS = {
    1: "HUP", 2: "INT", 3: "QUIT", 4: "ILL", 5: "TRAP", 6: "ABRT", 7: "BUS",
    8: "FPE", 9: "KILL", 10: "USR1", 11: "SEGV", 12: "USR2", 13: "PIPE",
    14: "ALRM", 15: "TERM", 17: "CHLD", 18: "CONT", 19: "STOP", 20: "TSTP",
    21: "TTIN", 22: "TTOU", 23: "URG", 24: "XCPU", 25: "XFSZ", 26: "VTALRM",
    27: "PROF", 28: "WINCH", 29: "IO", 30: "PWR", 31: "SYS",
}
#: musl: _NSIG is 65, SIGRTMIN is 35 (two below are reserved), SIGRTMAX 64.
MUSL_NSIG, MUSL_RTMIN, MUSL_RTMAX = 65, 35, 64
#: mksignames' cap on real-time signals (RTLIM).
RTLIM = 256


def signal_names(nsig: int = MUSL_NSIG, rtmin: int = MUSL_RTMIN, rtmax: int = MUSL_RTMAX,
                 named: dict[int, str] = LINUX_SIGNALS) -> list[str]:
    """mksignames' `initialize_signames`, for the given target's numbers."""
    names: dict[int, str] = {0: "EXIT", rtmin: "RTMIN", rtmax: "RTMAX"}
    if rtmax > rtmin:
        count = min((rtmax - rtmin - 1) // 2, RTLIM // 2 - 1)
        for i in range(1, count + 1):
            names[rtmin + i] = f"RTMIN+{i}"
            names[rtmax - i] = f"RTMAX-{i}"
        if count < RTLIM // 2 - 1 and count != (rtmax - rtmin) // 2:
            names[rtmin + count + 1] = f"RTMIN+{count + 1}"
    names.update(named)
    return [names.get(i, str(i)) for i in range(nsig)]


def signames_c(names: list[str] | None = None) -> str:
    names = signal_names() if names is None else names
    out = ["/* This file was automatically created by mksignames.\n",
           "   Do not edit.  Edit support/mksignames.c instead. */\n\n",
           "#include <signal.h>\n\n",
           "/* A translation list so we can be polite to our users. */\n",
           "const char *const signal_names[NSIG + 1] = {\n"]
    out += [f'    "{name}",\n' for name in names]
    out.append("    (char *)0x0\n};\n")
    return "".join(out)
