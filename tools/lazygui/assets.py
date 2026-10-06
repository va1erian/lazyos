"""Extra data asset trees as an image build switch (`LAZYOS_ASSETS`, issue #454).

Shared by the GUI (`catalog.py`, the Advanced tab's *Asset dirs* field) and
`tools/run_demo.py` (`--assets DIR`, repeatable). Each directory is copied to
`/system/share/<same relative path>` beside the checked-in `assets/` tree and
needs the same `manifest.txt` (`path | licence | install | provenance` per
file); the image build (`build_support/assets_embed.rs`) checks it and fails
with the reason, so this module only refuses what can never work.
"""

from __future__ import annotations

import os
from pathlib import Path

#: The manifest every asset tree carries at its root.
MANIFEST = "manifest.txt"


def check_dirs(dirs) -> list[str]:
    """Absolute paths of ``dirs`` (a list, or one string of ``;``-separated
    entries as the GUI field holds). Raises ``ValueError`` for a directory
    that does not exist or has no manifest."""
    if isinstance(dirs, str):
        dirs = dirs.split(";")
    checked = []
    for entry in dirs:
        entry = entry.strip()
        if not entry:
            continue
        path = Path(entry).expanduser().resolve()
        if not path.is_dir():
            raise ValueError(f"asset directory {entry!r} does not exist")
        if not (path / MANIFEST).is_file():
            raise ValueError(f"asset directory {entry!r} has no {MANIFEST} "
                             "(one `path | licence | install | provenance` line per file)")
        checked.append(str(path))
    return checked


def assets_env(dirs) -> dict[str, str]:
    """`LAZYOS_ASSETS` for ``dirs`` (none when there are none)."""
    checked = check_dirs(dirs)
    return {"LAZYOS_ASSETS": os.pathsep.join(checked)} if checked else {}


def add_assets_option(parser) -> None:
    """`--assets DIR` (repeatable) on an argparse parser."""
    parser.add_argument("--assets", action="append", default=[], metavar="DIR",
                        help=f"also copy DIR (with its {MANIFEST}) to /system/share "
                             "(LAZYOS_ASSETS; repeatable)")


def build_assets(args) -> dict[str, str]:
    """The environment for `--assets`, refusing it without a build."""
    env = assets_env(args.assets)
    if env and args.no_build:
        raise ValueError("--assets needs a build: the files are written into the image")
    return env


def assets_argv(cfg: dict) -> list[str]:
    """run_demo's `--assets` arguments for the GUI's settings."""
    if cfg.get("skip_build"):
        return []
    return [arg for path in check_dirs(cfg.get("assets", "")) for arg in ("--assets", path)]
