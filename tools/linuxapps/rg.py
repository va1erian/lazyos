"""Build ripgrep 14.1.1 as a static musl program.

Why not plain ``cargo install ripgrep --locked``: on musl, ripgrep links
jemalloc (``tikv-jemallocator``, a dependency for every 64-bit musl target,
not a feature). ``jemalloc-sys`` builds jemalloc with its autoconf script and
``make``, which cannot cross-compile from a Windows host (its build script
passes ``--build=x86_64-pc-win32``, which ``config.sub`` rejects). jemalloc
would also be the riskiest part of the program for LazyOS (background
threads, ``madvise`` tricks). So the published crate is fetched (pinned by
the crates.io checksum, ``sources.PINS["rg"]``) and three edits remove
jemalloc, after which ripgrep uses musl's malloc through Rust's system
allocator (upstream's choice on every non-musl target):

* ``Cargo.toml``: drop the musl-only ``jemallocator`` dependency;
* ``crates/core/main.rs``: drop the ``#[global_allocator]`` that names it;
* ``Cargo.lock``: drop the two jemalloc packages, so ``--locked`` still holds
  for every remaining dependency (each pinned by its checksum).

ripgrep's default features are pure Rust (PCRE2 is opt-in), so no C compiler
is involved. Linking reuses ``tools/rhai/build.py``: on Windows the musl
target has no host linker, so cargo uses the toolchain's bundled ``rust-lld``
with self-contained linking (Rust's own musl start files and libc).
"""

from __future__ import annotations

import importlib.util
import shutil
import subprocess
from pathlib import Path

import sources
from toolchain import BuildError

ROOT = Path(__file__).resolve().parent.parent.parent
TARGET = "x86_64-unknown-linux-musl"
#: A target directory of its own, kept between runs so a rebuild is quick.
CARGO_TARGET_DIR = sources.WORK / "cargo"

#: (file, exact upstream text, replacement). Each must match exactly once.
JEMALLOC_EDITS = [
    ("Cargo.toml",
     "[target.'cfg(all(target_env = \"musl\", target_pointer_width = \"64\"))'"
     ".dependencies.jemallocator]\nversion = \"0.5.0\"\n",
     ""),
    ("crates/core/main.rs",
     "#[cfg(all(target_env = \"musl\", target_pointer_width = \"64\"))]\n"
     "#[global_allocator]\n"
     "static ALLOC: jemallocator::Jemalloc = jemallocator::Jemalloc;\n",
     "// LazyOS build: jemalloc removed (tools/linuxapps/rg.py); musl's malloc is used.\n"),
    ("Cargo.lock",
     "[[package]]\n"
     "name = \"jemalloc-sys\"\n"
     "version = \"0.5.4+5.3.0-patched\"\n"
     "source = \"registry+https://github.com/rust-lang/crates.io-index\"\n"
     "checksum = \"ac6c1946e1cea1788cbfde01c993b52a10e2da07f4bac608228d1bed20bfebf2\"\n"
     "dependencies = [\n \"cc\",\n \"libc\",\n]\n\n"
     "[[package]]\n"
     "name = \"jemallocator\"\n"
     "version = \"0.5.4\"\n"
     "source = \"registry+https://github.com/rust-lang/crates.io-index\"\n"
     "checksum = \"a0de374a9f8e63150e6f5e8a60cc14c668226d7a347d8aee1a45766e3c4dd3bc\"\n"
     "dependencies = [\n \"jemalloc-sys\",\n \"libc\",\n]\n\n",
     ""),
    ("Cargo.lock", ' "ignore",\n "jemallocator",\n', ' "ignore",\n'),
]


def apply_edits(tree: Path, edits: list[tuple[str, str, str]]) -> None:
    """Apply each (file, old, new) edit under `tree`; `old` must occur once."""
    for name, old, new in edits:
        path = tree / name
        text = path.read_text(encoding="utf-8")
        if text.count(old) != 1:
            raise BuildError(f"rg: {name} does not contain the expected text once:\n{old}")
        path.write_text(text.replace(old, new), encoding="utf-8", newline="\n")


def _rhai_build():
    """tools/rhai/build.py as a module (its name, `build`, is taken here)."""
    path = ROOT / "tools" / "rhai" / "build.py"
    spec = importlib.util.spec_from_file_location("rhai_build", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def build_rg(out: Path) -> Path | None:
    """`out` holding a static rg, or None when Rust or the target is missing."""
    rhai = _rhai_build()
    if shutil.which("cargo") is None:
        sources.log("cargo not found; rg unavailable")
        return None
    if not rhai.ensure_target():
        return None
    tree = sources.work_copy("rg")
    if tree is None:
        return None
    apply_edits(tree, JEMALLOC_EDITS)
    # The copy sits under the LazyOS workspace's root manifest; an empty
    # `[workspace]` table makes it a workspace of its own (cargo's advice).
    with open(tree / "Cargo.toml", "a", encoding="utf-8", newline="\n") as manifest:
        manifest.write("\n# LazyOS build: not part of the enclosing workspace.\n[workspace]\n")
    env = rhai.build_env()
    env["CARGO_TARGET_DIR"] = str(CARGO_TARGET_DIR)
    # ripgrep's release profile keeps line tables (`debug = 1`): 30 MB.
    # Stripped, like the C programs, it is a fraction of that on the disk image.
    env["CARGO_PROFILE_RELEASE_STRIP"] = "symbols"
    # ripgrep's build.rs embeds `git rev-parse HEAD` when it runs inside a git
    # checkout; this copy sits inside the LazyOS repository, whose hash it
    # must not claim. Stop git's search at the copy itself.
    env["GIT_CEILING_DIRECTORIES"] = str(tree.parent)
    command = ["cargo", "build", "--release", "--locked", "--bin", "rg",
               "--target", TARGET]
    sources.log(f"cargo build ripgrep {sources.PINS['rg'].version} (--locked, {TARGET})")
    done = subprocess.run(command, cwd=tree, env=env, capture_output=True, text=True)
    if done.returncode != 0:
        if rhai.no_linker(done.stderr):
            sources.log("no linker for the musl target; rg unavailable")
            return None
        if any(sign in done.stderr for sign in
               ("failed to download", "failed to query", "spurious network error")):
            sources.log("crates.io unreachable; rg unavailable")
            return None
        raise BuildError(f"building ripgrep failed:\n{done.stderr[-4000:]}")
    built = CARGO_TARGET_DIR / TARGET / "release" / "rg"
    if not built.is_file():
        raise BuildError(f"{built} was not produced")
    out.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(built, out)
    return out

