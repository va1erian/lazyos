"""Helpers every recipe uses: the zig toolchain environment, CMake and
autoconf runners, and the stamps that make a rebuild skip finished libraries.

Everything ends up in the one prefix ``wkpins.PREFIX``, but a library never
installs into it directly: its install step writes under ``DESTDIR`` (a staging
directory, ``stage()``), and leaving the :class:`Recipe` copies the staged files
into the prefix. So the files a library owns are exactly what its install step
wrote, whatever the prefix already held: a stamp
(``<prefix>/.stamps/<name>.json``) holds the recipe's key (source digest, the
configuration, the stamps of what it depends on) and that file list (what
``manifest.py`` reports), and a rebuild first deletes the files of the library's
previous install, so nothing of an old version is left behind.
"""

from __future__ import annotations

import hashlib
import json
import os
import shlex
import shutil
import zipfile
from pathlib import Path

import wkpins  # first: it puts tools/jsc (toolenv) on sys.path
import toolenv
from toolenv import BuildError, run

PREFIX = wkpins.PREFIX
STAMPS = PREFIX / ".stamps"
#: Bump to rebuild everything after a change to how libraries are configured.
RECIPE_REV = "1"


def flags() -> str:
    return " ".join(toolenv.CODEGEN)


def tool_env() -> dict[str, str]:
    """Compilers and search paths for autoconf and Makefile builds."""
    wrap = toolenv.wrappers()
    lib = PREFIX / "lib"
    pkg = f"{lib}/pkgconfig:{PREFIX}/share/pkgconfig"
    return {
        "CC": wrap["zcc"], "CXX": wrap["zc++"], "AR": wrap["zar"], "RANLIB": wrap["zranlib"],
        "CFLAGS": f"{flags()} -I{PREFIX}/include",
        "CXXFLAGS": f"{flags()} -I{PREFIX}/include",
        "CPPFLAGS": f"-I{PREFIX}/include",
        "LDFLAGS": f"-L{lib}",
        "PKG_CONFIG_PATH": pkg, "PKG_CONFIG_LIBDIR": pkg,
        # Static archives list their dependencies only under Requires.private.
        "PKG_CONFIG": "pkg-config --static",
    }


def pkg_env() -> dict[str, str]:
    """pkg-config sees this prefix only (CMake's FindPkgConfig-based modules)."""
    pkg = f"{PREFIX}/lib/pkgconfig:{PREFIX}/share/pkgconfig"
    return {"PKG_CONFIG_PATH": pkg, "PKG_CONFIG_LIBDIR": pkg}


def cmake_args(options: dict[str, str]) -> list[str]:
    """The arguments every CMake project gets, then the project's own."""
    wrap = toolenv.wrappers()
    common = {
        "CMAKE_BUILD_TYPE": "Release", "CMAKE_INSTALL_PREFIX": str(PREFIX),
        "CMAKE_INSTALL_LIBDIR": "lib",
        "CMAKE_C_COMPILER": wrap["zcc"], "CMAKE_CXX_COMPILER": wrap["zc++"],
        "CMAKE_AR": wrap["zar"], "CMAKE_RANLIB": wrap["zranlib"],
        "CMAKE_C_FLAGS": f"{flags()} -I{PREFIX}/include",
        "CMAKE_CXX_FLAGS": f"{flags()} -I{PREFIX}/include",
        # Release's own -O3 would follow our -O2.
        "CMAKE_C_FLAGS_RELEASE": "-DNDEBUG", "CMAKE_CXX_FLAGS_RELEASE": "-DNDEBUG",
        "CMAKE_POSITION_INDEPENDENT_CODE": "ON", "BUILD_SHARED_LIBS": "OFF",
        "CMAKE_PREFIX_PATH": str(PREFIX), "CMAKE_FIND_ROOT_PATH": str(PREFIX),
        # Only this prefix: a library of the build container must never be found.
        "CMAKE_FIND_ROOT_PATH_MODE_LIBRARY": "ONLY", "CMAKE_FIND_ROOT_PATH_MODE_INCLUDE": "ONLY",
        "CMAKE_FIND_ROOT_PATH_MODE_PACKAGE": "ONLY",
        "CMAKE_POLICY_VERSION_MINIMUM": "3.5",
    }
    common.update(options)
    return [f"-D{key}={value}" for key, value in common.items()]


def key_of(source_sha: str, config: object, deps: list[str]) -> str:
    """What a stamp must equal for the library to be up to date."""
    parts = [RECIPE_REV, source_sha, json.dumps(config, sort_keys=True, default=str)]
    for dep in deps:
        parts.append(stamp_key(dep) or "missing")
    return hashlib.sha256("\n".join(parts).encode()).hexdigest()[:20]


def stamp_key(name: str) -> str | None:
    path = STAMPS / f"{name}.json"
    return json.loads(path.read_text())["key"] if path.is_file() else None


def is_current(name: str, key: str) -> bool:
    return stamp_key(name) == key


#: Where installs are staged: ``<stage root>/<library>`` is a DESTDIR.
STAGE_ROOT = wkpins.WORK / "stage"
#: The library whose install is running (set by :class:`Recipe`).
_current: "Recipe | None" = None


def list_tree(root: Path) -> list[str]:
    """Every file and symlink under `root` (never the stamps), relative, sorted."""
    found: set[str] = set()
    if not root.is_dir():
        return []
    for here, dirs, files in os.walk(root):
        if Path(here) == root and ".stamps" in dirs:
            dirs.remove(".stamps")
        for name in files:
            found.add((Path(here) / name).relative_to(root).as_posix())
        for name in dirs:  # symlinked directories count as files
            link = Path(here) / name
            if link.is_symlink():
                found.add(link.relative_to(root).as_posix())
    return sorted(found)


def under_root(path: Path) -> Path:
    """`path` without its root (and drive), to join under a DESTDIR."""
    return Path(*path.parts[1:])


def stage() -> Path:
    """The DESTDIR of the library being built (an install step passes it on)."""
    if _current is None:
        raise BuildError("no library is being built: nothing to stage into")
    return _current.stage


def staged_prefix() -> Path:
    """Where the staged files of the library being built sit: `PREFIX` under the DESTDIR."""
    return stage() / under_root(PREFIX)


def destdir_env() -> dict[str, str]:
    """The environment that makes ``cmake --install`` stage."""
    return {"DESTDIR": str(stage())}


def remove_installed(files: list[str]) -> None:
    """Delete the prefix files of a previous install, then the directories it emptied."""
    parents: set[Path] = set()
    for rel in files:
        path = PREFIX / rel
        if path.is_symlink() or path.is_file():
            path.unlink()
        parents.update(p for p in path.parents if PREFIX in p.parents)
    for directory in sorted(parents, key=lambda p: len(p.parts), reverse=True):
        try:
            directory.rmdir()  # only an empty one goes
        except OSError:
            pass


def read_stamp(name: str) -> dict | None:
    path = STAMPS / f"{name}.json"
    return json.loads(path.read_text()) if path.is_file() else None


class Recipe:
    """Context for one library: stages its install, then moves it into the
    prefix, replacing the files of the library's previous install, and writes
    its stamp with the files the install step wrote."""

    def __init__(self, dep: wkpins.Dep | None, name: str, key: str, version: str = "",
                 info: dict | None = None) -> None:
        self.dep, self.name, self.key = dep, name, key
        self.version = version or (dep.source.version if dep else "")
        self.info = info or {}
        self.stage = STAGE_ROOT / name

    def __enter__(self) -> "Recipe":
        global _current
        # What the library owned is gone before it builds, so it only ever
        # sees the files of its dependencies; a failed build leaves no stamp
        # (and no half of an old version) for the next run to trust.
        previous = read_stamp(self.name)
        if previous:
            remove_installed(previous.get("files", []))
            (STAMPS / f"{self.name}.json").unlink()
        shutil.rmtree(self.stage, ignore_errors=True)
        self.stage.mkdir(parents=True)
        _current = self
        return self

    def __exit__(self, kind, value, trace) -> None:
        global _current
        _current = None
        try:
            if kind is None:
                self.commit()
        finally:
            shutil.rmtree(self.stage, ignore_errors=True)

    def commit(self) -> None:
        """Copy the staged files into the prefix and write the stamp."""
        staged = self.stage / under_root(PREFIX)
        written = list_tree(staged)
        if staged.is_dir():
            shutil.copytree(staged, PREFIX, symlinks=True, dirs_exist_ok=True)
        STAMPS.mkdir(parents=True, exist_ok=True)
        record = {"name": self.name, "version": self.version, "key": self.key, "files": written}
        if self.dep:
            record.update(archive=self.dep.archive, url=self.dep.source.url,
                          sha256=self.dep.source.sha256, license=self.dep.license, why=self.dep.why)
            if self.dep.notes:
                record["notes"] = self.dep.notes
        record.update(self.info)
        (STAMPS / f"{self.name}.json").write_text(json.dumps(record, indent=1))


def source_tree(dep: wkpins.Dep) -> Path:
    """Fetch (verified), unpack once, apply ``patches/<name>/*.patch``; the tree."""
    archive = wkpins.fetch(dep)
    if archive is None:
        raise BuildError(f"{dep.name}: download unavailable or its digest differs")
    base = wkpins.WORK / "src"
    if archive.suffix == ".zip":
        tree = base / dep.source.topdir
        mark = tree / ".lazyos-extracted"
        if not mark.is_file():
            shutil.rmtree(tree, ignore_errors=True)
            base.mkdir(parents=True, exist_ok=True)
            with zipfile.ZipFile(archive) as zf:
                zf.extractall(base)
            toolenv.one_time(tree)
            mark.write_text("ok\n")
    else:
        tree = toolenv.extract(archive, dep.source, base)
    apply_patches(tree, dep.name)
    return tree


def apply_patches(tree: Path, name: str) -> list[str]:
    """Apply ``tools/webkit/patches/<name>/*.patch`` in name order, once each."""
    patch_dir = wkpins.HERE / "patches" / name
    names = sorted(p.name for p in patch_dir.glob("*.patch")) if patch_dir.is_dir() else []
    done = tree / ".lazyos-patched"
    applied = done.read_text().split() if done.is_file() else []
    for patch in names:
        if patch in applied:
            continue
        run(["patch", "-p1", "--forward", "-i", str(patch_dir / patch)], tree, f"patching {name} {patch}")
        applied.append(patch)
        done.write_text("\n".join(applied) + "\n")
    return names


def patch_key(name: str) -> list[str]:
    """The contents of a library's patches, so editing one rebuilds it."""
    patch_dir = wkpins.HERE / "patches" / name
    if not patch_dir.is_dir():
        return []
    return [hashlib.sha256(p.read_bytes()).hexdigest()[:12] for p in sorted(patch_dir.glob("*.patch"))]


def cmake_build(name: str, tree: Path, options: dict[str, str], jobs: int,
                subdir: str = "", env: dict[str, str] | None = None) -> None:
    """Configure and build a CMake project, and install it into the staging directory."""
    build = wkpins.BUILD / name
    shutil.rmtree(build, ignore_errors=True)
    build.mkdir(parents=True)
    source = tree / subdir if subdir else tree
    env = {**pkg_env(), **(env or {})}
    run(["cmake", "-G", "Ninja", "-S", str(source), "-B", str(build), *cmake_args(options)],
        build, f"{name} configure", env=env)
    run(["cmake", "--build", str(build), "-j", str(jobs)], build, f"{name} build", env=env)
    run(["cmake", "--install", str(build)], build, f"{name} install", env={**env, **destdir_env()})


def configure_build(name: str, tree: Path, args: list[str], jobs: int,
                    env: dict[str, str] | None = None, subdir: str = "") -> Path:
    """``configure && make && make install DESTDIR=<stage>`` out of tree; the build directory."""
    build = wkpins.BUILD / name
    shutil.rmtree(build, ignore_errors=True)
    build.mkdir(parents=True)
    merged = tool_env()
    merged.update(env or {})
    source = tree / subdir if subdir else tree
    run([str(source / "configure"), f"--prefix={PREFIX}", "--libdir=" + str(PREFIX / "lib"),
         *args], build, f"{name} configure", env=merged)
    run(["make", f"-j{jobs}"], build, f"{name} make", env=merged)
    run(["make", "install", f"DESTDIR={stage()}"], build, f"{name} make install", env=merged)
    return build


def describe(command: list[str]) -> str:
    return " ".join(shlex.quote(c) for c in command)
