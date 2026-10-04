#!/usr/bin/env python3
"""Make the pinned xui git dependency check out on Windows.

``xui``'s ``xui-netsurf`` crate vendors NetSurf's libraries as git submodules.
One of them, ``libnsbmp``, ships AFL test cases whose names contain a colon
(``test/afl-bmp/id:000023,...bmp``). Windows forbids ``:`` in a file name, so
libgit2 refuses to check the submodule out::

    failed to update submodule `crates/xui-netsurf/netsurf-sys/vendor/libnsbmp`
    cannot checkout to invalid path 'test/afl-bmp/id:000023,...bmp'

Because cargo updates every submodule named in the dependency's ``.gitmodules``
(whether the built crate needs it or not), the whole ``xui`` dependency fails to
resolve, and with it the app build.

This script removes that obstacle:

* It checks out every submodule the dependency declares, leaving out the
  colon-named AFL test directory. NetSurf builds the libraries' ``src`` only, so
  those tests are dead weight. (Cargo stops at the first submodule it cannot
  check out, so the ones after ``libnsbmp`` are missing too and must be filled
  in as well.)
* It writes cargo's ``.cargo-ok`` marker so cargo uses the checkout as-is
  instead of re-cloning (and re-tripping over the submodule) on the next
  resolve.  Cargo still fetches the revision first, so a moved pin re-seeds.
* It clears any ``update = none`` setting an older run left, so every submodule
  is materialised rather than skipped.
* It seeds *every* cargo checkout of the repository. Cargo keys git sources by
  URL and two spellings are two sources: ``xui-app`` and ``doom`` use
  ``https://github.com/va1erian/xui`` while ``lazyrad-os`` patches in
  ``https://www.github.com/va1erian/xui``. Both trip the same way and both must
  be fixed.

Cargo's own git checkout is used on purpose: a ``[patch]`` vendoring the crates
into this repository would change identities and paths (the built ELF's
debuginfo) and is a much bigger change than the build needs.

On non-Windows hosts, or when a checkout is already seeded, this is a no-op.
Run it through the build scripts (``tools/xui/build.py``,
``tools/lazyrad/build.py``, ``tools/doom/build.py``); it needs ``git`` on
``PATH``.
"""

from __future__ import annotations

import os
import subprocess
import sys
from pathlib import Path

# One xui revision for every crate the apps build (`xui-app/Cargo.toml`, docs,
# web, LazyWriter, LazyRAD). Keep in step with those manifests.
XUI_REV = "c7cd6d0838be293063f887ed9d8043fbe6143e4f"
XUI_URL = "https://github.com/va1erian/xui"
# The directory whose files Windows cannot name must be left out of every
# submodule (NetSurf's libraries carry AFL corpora with `:`-names).
SKIP_PATHS = ["test"]


def git_dir() -> Path:
    """Cargo's git directory (``$CARGO_HOME/git``, ``~/.cargo/git`` by default)."""
    cargo_home = os.environ.get("CARGO_HOME")
    base = Path(cargo_home) if cargo_home else Path.home() / ".cargo"
    return base / "git"


def find_checkouts(git: Path, rev: str) -> list[Path]:
    """Every cargo checkout of the xui repository at ``rev``.

    Cargo keys its git sources by URL, and two spellings of the same repository
    are two sources: `xui-app` and `doom` use ``https://github.com/va1erian/xui``
    while `lazyrad-os` patches in ``https://www.github.com/...``. Both need the
    submodule fixup, so return them all.
    """
    db_root = git / "db"
    if not db_root.is_dir():
        return []
    found: list[Path] = []
    for db in sorted(db_root.iterdir()):
        if not (db / "config").is_file():
            continue
        probe = subprocess.run(
            ["git", "--git-dir", str(db), "cat-file", "-e", rev],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        if probe.returncode != 0:
            continue
        if _db_is_xui(db, rev):
            found.append(git / "checkouts" / db.name / rev[:7])
    return found


def _db_is_xui(db: Path, rev: str) -> bool:
    """True when the DB's tree at ``rev`` is xui (cargo's bare repos carry no
    URL to match on, so the layout is the check)."""
    out = subprocess.run(
        ["git", "--git-dir", str(db), "ls-tree", "--name-only", rev],
        capture_output=True, text=True,
    ).stdout
    names = set(out.split())
    return "crates" in names and "Cargo.toml" in names


def checkout_seeded(checkout: Path) -> bool:
    """True when the checkout has the sources and every submodule populated at
    its pinned commit."""
    if not (checkout / ".cargo-ok").is_file():
        return False
    if not (checkout / "crates/xui-core/src").is_dir():
        return False
    for _name, path, _url in _submodules(checkout):
        if not _submodule_matches(checkout, path):
            return False
    return True


def _submodule_matches(checkout: Path, path: str) -> bool:
    """True when the submodule is populated and its non-`test` tree matches the
    superproject's pinned gitlink (so a stale checkout is re-seeded)."""
    dest = checkout / path
    if not (dest / ".git").exists():
        return False
    if not any(entry.name != ".git" for entry in dest.iterdir()):
        return False
    sha = subprocess.run(
        ["git", "-C", str(checkout), "rev-parse", f":{path}"],
        capture_output=True, text=True,
    )
    if sha.returncode != 0:
        return False
    pathspec = ["."] + [":!" + p for p in SKIP_PATHS]
    diff = subprocess.run(
        ["git", "-C", str(dest), "diff", "--quiet", sha.stdout.strip(), "--"] + pathspec,
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    )
    return diff.returncode == 0


def seed(checkout: Path) -> None:
    """Materialise the pinned xui sources and every submodule in an existing
    cargo git checkout."""
    checkout.parent.mkdir(parents=True, exist_ok=True)
    if not (checkout / ".git").exists():
        if checkout.exists():
            _rmtree(checkout)
        subprocess.run(["git", "clone", "--quiet", str(_db_for(checkout)), str(checkout)], check=True)
    run = ["git", "-C", str(checkout)]
    subprocess.run(run + ["checkout", "--quiet", "--force", XUI_REV], check=True)
    for name, path, url in _submodules(checkout):
        # `update = none` would make cargo skip the submodule on its next resolve.
        subprocess.run(run + ["config", "--unset-all", f"submodule.{name}.update"],
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        _checkout_submodule(checkout, path, url)
    (checkout / ".cargo-ok").write_text("")
    print(f"xui: seeded {checkout}", file=sys.stderr)


def _submodules(checkout: Path) -> list[tuple[str, str, str]]:
    """Every submodule in the checkout's `.gitmodules`, as
    ``(name, path, url)``; empty when the file or URLs are unreadable."""
    out = subprocess.run(
        ["git", "-C", str(checkout), "config", "-f", ".gitmodules",
         "--get-regexp", r"^submodule\..*\.path$"],
        capture_output=True, text=True,
    ).stdout
    found: list[tuple[str, str, str]] = []
    for line in out.splitlines():
        key, _, path = line.partition(" ")
        name = key[len("submodule."):-len(".path")]
        url = subprocess.run(
            ["git", "-C", str(checkout), "config", "-f", ".gitmodules",
             "--get", f"submodule.{name}.url"],
            capture_output=True, text=True,
        ).stdout.strip()
        found.append((name, path, url))
    return found


def _db_for(checkout: Path) -> Path:
    """The bare repo cargo fetched the revision into.

    Cargo keeps it in ``git/db``, a sibling of the ``git/checkouts`` tree, so
    from ``git/checkouts/<name>/<rev>`` go up two levels and across.
    """
    return checkout.parents[1].parent / "db" / checkout.parent.name


def _submodule_url(checkout: Path, name: str, fallback: str) -> str:
    """The submodule's URL, from the superproject's config or its .gitmodules."""
    for source in (["--get", f"submodule.{name}.url"],
                   ["-f", ".gitmodules", "--get", f"submodule.{name}.url"]):
        url = subprocess.run(
            ["git", "-C", str(checkout), "config", *source],
            capture_output=True, text=True,
        ).stdout.strip()
        if url:
            return url
    return fallback


def _checkout_submodule(checkout: Path, path: str, url: str) -> None:
    """Check one submodule out at its pinned commit, skipping the colon-named
    paths Windows cannot name. A submodule already at its pinned tree is left
    alone; a stale or empty one is re-materialised."""
    dest = checkout / path
    if _submodule_matches(checkout, path):
        return
    if dest.exists() and not (dest / ".git").exists():
        _rmtree(dest)
    dest.parent.mkdir(parents=True, exist_ok=True)
    if not (dest / ".git").exists():
        url = _submodule_url(checkout, path, url) or f"{XUI_URL}.git"
        subprocess.run(["git", "clone", "--quiet", "--no-checkout", url, str(dest)], check=True)
    sha = subprocess.run(
        ["git", "-C", str(checkout), "rev-parse", f":{path}"],
        capture_output=True, text=True, check=True,
    ).stdout.strip()
    pathspec = ["."] + [":!" + p for p in SKIP_PATHS]
    subprocess.run(
        ["git", "-C", str(dest), "checkout", "--quiet", "--force", sha, "--"] + pathspec,
        check=True,
    )
    print(f"xui: checked out {path} without {'/'.join(SKIP_PATHS)}", file=sys.stderr)


def _rmtree(path: Path) -> None:
    """Remove a tree, clearing the read-only bit git sets on the way."""
    import shutil

    def onerror(func, p, _exc):
        """Retry a failed removal after clearing the read-only bit."""
        os.chmod(p, 0o700)
        func(p)

    shutil.rmtree(path, onerror=onerror)


def resolve(manifest: Path, env: dict[str, str] | None = None) -> bool:
    """Resolve ``manifest``'s dependencies, seeding the xui checkouts on Windows
    if the first attempt trips over NetSurf's colon-named submodule files.

    The first probe makes cargo fetch the pinned revision (its bare repos then
    hold the sources) and fails at the submodule checkout; this fixup seeds
    them; a second probe confirms the graph resolves. A no-op where it is not
    needed.
    """
    def probe() -> subprocess.CompletedProcess:
        """Run `cargo metadata` for the manifest, capturing its diagnostics."""
        return subprocess.run(
            ["cargo", "metadata", "--manifest-path", str(manifest),
             "--format-version", "1"],
            capture_output=True, text=True, env=env,
        )

    first = probe()
    if first.returncode == 0:
        return True
    if os.name != "nt":
        print(first.stderr.strip(), file=sys.stderr)
        return False
    if not ensure_xui_checkout():
        print(first.stderr.strip(), file=sys.stderr)
        return False
    second = probe()
    if second.returncode != 0:
        print(second.stderr.strip(), file=sys.stderr)
        return False
    print("xui: seeded the git checkout for Windows", file=sys.stderr)
    return True


def ensure_xui_checkout(rev: str = XUI_REV) -> bool:
    """Make cargo's checkouts of the xui repository usable on Windows. Returns
    True if all are usable (or the fix is not needed on this host)."""
    if os.name != "nt":
        return True
    git = git_dir()
    # Cargo fetches the revision itself when it resolves the dependency; once it
    # has, its bare repos are here and the checkouts can be seeded. At least one
    # URL spelling of the repository must be present for the fix to matter.
    checkouts = find_checkouts(git, rev)
    if not checkouts:
        return False
    ok = True
    for checkout in checkouts:
        if checkout_seeded(checkout):
            continue
        try:
            seed(checkout)
        except subprocess.CalledProcessError as error:
            print(f"warning: could not seed the xui checkout {checkout}: {error}",
                  file=sys.stderr)
            ok = False
            continue
        ok = checkout_seeded(checkout) and ok
    return ok


def main() -> int:
    """Seed the xui checkouts; exit 0 when they are ready, 1 when cargo must
    fetch the revision first."""
    if ensure_xui_checkout():
        print("xui checkout is ready")
        return 0
    print("xui checkout is not ready yet (cargo must fetch it first)", file=sys.stderr)
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
