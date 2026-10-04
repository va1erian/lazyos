"""The build steps of the optional apps an image can embed (LazyRAD, the MOD
player package, Doom, the Linux programs, the HTTPS tools, LazyWeb), run before `cargo build`.
`catalog` re-exports them; they live apart to keep it small."""

from __future__ import annotations

import sys

PY = sys.executable


def lazyrad_step(cfg: dict) -> list[dict]:
    """The step that builds LazyRAD's static-musl ELFs, when the image embeds them."""
    if not cfg.get("lazyrad") and not cfg.get("modplayer"):
        return []
    return [{"label": "Build LazyRAD (static musl)",
             "argv": [PY, "tools/lazyrad/build.py"]}]


def doom_step(cfg: dict) -> list[dict]:
    """The step that builds the Doom package, when the image embeds it."""
    if not cfg.get("doom"):
        return []
    return [{"label": "Build Doom package (engine + Freedoom)",
             "argv": [PY, "tools/doom/build.py", "--require"]}]


def modplayer_step(cfg: dict) -> list[dict]:
    """The step that packages the LazyRAD MOD player, when the image embeds it
    (after `lazyrad_step`: the package carries the player it built)."""
    if not cfg.get("modplayer"):
        return []
    return [{"label": "Package the MOD player (LazyRAD)",
             "argv": [PY, "tools/lazyrad/package.py", "--no-build", "--require"]}]


def linuxapps_step(cfg: dict) -> list[dict]:
    """The step that builds the Linux command-line programs, when embedded."""
    if not cfg.get("linuxapps"):
        return []
    return [{"label": "Build Linux programs (dash, lua, sqlite3, jq, rg)",
             "argv": [PY, "tools/linuxapps/build.py", "--require"]}]


def tls_step(cfg: dict) -> list[dict]:
    """The step that builds the HTTPS clients (`fetch`, `curl`, `wget`), when embedded."""
    if not cfg.get("tls"):
        return []
    return [{"label": "Build HTTPS tools (curl, wget, fetch)",
             "argv": [PY, "tools/nettls/build.py", "--require"]}]


def lazyweb_step(cfg: dict) -> list[dict]:
    """The step that builds the LazyWeb browser (with the other xui apps and
    their core packages; NetSurf needs zig), when the image embeds it."""
    if not cfg.get("lazyweb"):
        return []
    return [{"label": "Build xui apps with LazyWeb (zig)", "argv": [PY, "tools/xui/build.py"]}]


def app_steps(cfg: dict) -> list[dict]:
    """Every optional app the image embeds, built before `cargo build`."""
    return (lazyrad_step(cfg) + modplayer_step(cfg) + doom_step(cfg) + linuxapps_step(cfg)
            + tls_step(cfg) + lazyweb_step(cfg))
