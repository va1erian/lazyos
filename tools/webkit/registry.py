"""The table of library recipes and the function that builds one with its
dependencies, skipping what a matching stamp says is already installed."""

from __future__ import annotations

from dataclasses import dataclass, field
from pathlib import Path
from typing import Callable

import buildkit
import wkpins


@dataclass
class Lib:
    name: str
    #: Libraries (recipe names) that must be installed first.
    needs: list[str]
    #: Configuration that decides the key: a rebuild happens when it changes.
    config: dict
    #: ``build(tree, jobs)`` installs into the prefix.
    build: Callable[[Path, int], None]
    #: Libraries with no pinned archive (ICU, Skia) build their own tree.
    own_tree: bool = False
    extra: dict = field(default_factory=dict)


LIBS: dict[str, Lib] = {}


def register(lib: Lib) -> Lib:
    LIBS[lib.name] = lib
    return lib


def ensure(name: str, jobs: int, done: set[str] | None = None) -> None:
    """Install `name` (and what it needs) unless its stamp is current."""
    done = set() if done is None else done
    if name in done:
        return
    lib = LIBS[name]
    for need in lib.needs:
        ensure(need, jobs, done)
    dep = wkpins.BY_NAME.get(name)
    source_sha = dep.source.sha256 if dep else lib.extra.get("source_sha", name)
    config = {"config": lib.config, "patches": buildkit.patch_key(name)}
    key = buildkit.key_of(source_sha, config, lib.needs)
    done.add(name)
    if buildkit.is_current(name, key):
        wkpins.log(f"{name}: up to date")
        return
    wkpins.log(f"{name}: building")
    with buildkit.Recipe(dep, name, key, lib.extra.get("version", ""), lib.extra.get("info")):
        tree = Path() if lib.own_tree else buildkit.source_tree(dep)
        lib.build(tree, jobs)
