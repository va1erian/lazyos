"""Software suites: named sets of `run_demo.py` flags (`tools/suites.toml`).

Pure and standard-library only (Python 3.11 `tomllib`), so `run_demo.py`, the
launcher, the stick maker and the update tooling read the same definition
(docs/update-plan.md decision 14). A suite names flags, never build steps: each
flag already runs its own prerequisite build.
"""

from __future__ import annotations

import sys
import tomllib
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from lazygui import osvolume  # noqa: E402

DEFAULT_PATH = Path(__file__).resolve().with_name("suites.toml")
#: Flags that bring the WebKit programs, which `osvolume` sizes the volume for.
WEBKIT_FLAGS = ("webkit", "webkit-port", "lazywebplus")


def load(path: Path = DEFAULT_PATH) -> dict:
    """The parsed and validated definition: `suites`, `options` and `test_only`.
    Raises `ValueError` on a malformed file, an unknown `extends`, or a flag that
    is both in a suite or option and in `test_only`."""
    with Path(path).open("rb") as handle:
        data = tomllib.load(handle)
    suites, options = data.get("suites", {}), data.get("options", {})
    test_only = data.get("test_only", {}).get("flags", [])
    if "base" not in suites or "full" not in suites:
        raise ValueError(f"{path}: suites `base` and `full` are required")
    for kind, table in (("suite", suites), ("option", options)):
        for name, entry in table.items():
            flags = entry.get("flags")
            if not isinstance(flags, list) or not all(isinstance(f, str) for f in flags):
                raise ValueError(f"{path}: {kind} {name!r} needs a `flags` list of strings")
            parent = entry.get("extends")
            if parent is not None and kind == "option":
                raise ValueError(f"{path}: option {name!r}: `extends` is for suites")
            if parent is not None and parent not in suites:
                raise ValueError(f"{path}: {kind} {name!r} extends unknown suite {parent!r}")
            if set(flags) & set(test_only):
                raise ValueError(f"{path}: {kind} {name!r} names test-only flags "
                                 f"{sorted(set(flags) & set(test_only))}")
    return {"suites": suites, "options": options, "test_only": list(test_only)}


def suite_names(data: dict | None = None) -> list[str]:
    """The suites, `base` first."""
    return list((data or load())["suites"])


def option_names(data: dict | None = None) -> list[str]:
    """The options `--with` takes."""
    return list((data or load())["options"])


def suite_flags(name: str, data: dict | None = None) -> list[str]:
    """A suite's flags, those of the suite it extends first (no duplicates)."""
    data = data or load()
    if name not in data["suites"]:
        raise ValueError(f"unknown suite {name!r}; known: {', '.join(data['suites'])}")
    entry, seen = data["suites"][name], [name]
    flags = entry["flags"]
    while entry.get("extends"):
        parent = entry["extends"]
        if parent in seen:
            raise ValueError(f"suite {name!r} extends itself through {parent!r}")
        seen.append(parent)
        entry = data["suites"][parent]
        flags = entry["flags"] + flags
    return list(dict.fromkeys(flags))


def resolve(name: str, options=(), data: dict | None = None) -> list[str]:
    """The `run_demo.py` flags (names without `--`) of a suite plus options."""
    data = data or load()
    flags = suite_flags(name, data)
    for option in options:
        if option not in data["options"]:
            raise ValueError(f"unknown option {option!r}; known: {', '.join(data['options'])}")
        flags += data["options"][option]["flags"]
    return list(dict.fromkeys(flags))


def dest(flag: str) -> str:
    """The argparse attribute of a flag name (`webkit-deps` -> `webkit_deps`)."""
    return flag.replace("-", "_")


def apply(args, flags) -> None:
    """Turn each flag on in a parsed `run_demo.py` namespace (explicit flags
    already there stay: a suite only adds)."""
    for flag in flags:
        setattr(args, dest(flag), True)


def os_size_bytes(flags) -> int:
    """The OS volume a flag set needs, by `osvolume`'s rules (one source of truth)."""
    flags = set(flags)
    return osvolume.required_bytes("srb2kart" in flags, bool(flags & set(WEBKIT_FLAGS)))


def describe(name: str, options=(), data: dict | None = None) -> str:
    """One line for logs: the suite, its options, the flag count and the OS size."""
    flags = resolve(name, options, data)
    label = name + "".join(f" +{option}" for option in options)
    return f"suite {label}: {len(flags)} flags, OS volume {os_size_bytes(flags) >> 20} MiB"
