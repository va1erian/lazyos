"""Kernel limits as image build switches (`LAZYOS_LIMIT_*` -> `limit.*` in `lazyos.cfg`).

Shared by the GUI (`catalog.py`) and `tools/run_demo.py`; the keys mirror
`kernel/src/limits.rs` (docs/architecture/limits.md) and the image build
validates the values themselves (`build_support/limits_cfg.rs`).
"""

from __future__ import annotations

#: The kernel limits `lazyos.cfg` can set; `KEY=VALUE` becomes `LAZYOS_LIMIT_<KEY>`.
LIMIT_KEYS = ("heap_max", "fd_max", "stack_size", "quota_user_memory",
              "quota_kernel_memory", "shared_buffer_max", "scratch_max", "task_max")


#: The "big app" profile (docs/architecture/limits.md): room for one
#: browser-class program (dozens of threads, a few hundred MB, thousands of
#: descriptors). `--big-app` fills in what the user did not choose.
BIG_APP_MEMORY = "4G"
BIG_APP_LIMITS = ("quota_user_memory=3G", "fd_max=8192", "task_max=1024")


#: The WebKit browser (docs/webkit-port-plan.md W5) on top of the big-app profile: the kernel charges
#: the *length* of every mapping to the user's memory quota, and the UI process, WebProcess and
#: NetworkProcess take what the quota lets them reserve: 2989 of the profile's 3072 MiB after the
#: first page and 3059 after seven (a second WebProcess's `exec` failed with ENOMEM), 6957 and 7027 of
#: 8192 at 8G. The charge is virtual: the guest still had 3405 of 4019 MiB of RAM free.
WEBKIT_LIMITS = ("quota_user_memory=8G",)


def with_defaults(entries, defaults) -> list[str]:
    """``entries`` plus each of ``defaults`` whose key ``entries`` does not already set."""
    chosen = {entry.partition("=")[0].strip().lower() for entry in entries}
    return [*entries, *(entry for entry in defaults if entry.partition("=")[0] not in chosen)]


def big_app(memory: str, default_memory: str, entries: list[str]) -> tuple[str, list[str]]:
    """``(memory, limits)`` for ``--big-app``: guest RAM unless one was chosen
    (``memory`` differs from ``default_memory``), and each of
    :data:`BIG_APP_LIMITS` unless ``entries`` already sets that key."""
    return (BIG_APP_MEMORY if memory == default_memory else memory), with_defaults(entries, BIG_APP_LIMITS)


def cfg_limit_env(cfg: dict) -> dict[str, str]:
    """:func:`limit_env` for a launcher configuration: its Advanced entries, and the WebKit
    browser's quota unless they set one."""
    entries = cfg.get("limits", "")
    entries = entries.split() if isinstance(entries, str) else list(entries or ())
    if cfg.get("webkit"):
        entries = with_defaults(entries, WEBKIT_LIMITS)
    return limit_env(entries)


def limit_env(entries) -> dict[str, str]:
    """`LAZYOS_LIMIT_*` variables for ``KEY=VALUE`` entries (a list, or one
    whitespace-separated string). Raises ``ValueError`` on an unknown key or
    an entry without ``=``; the image build checks the values themselves."""
    if isinstance(entries, str):
        entries = entries.split()
    env: dict[str, str] = {}
    for entry in entries or ():
        key, sep, value = entry.partition("=")
        key = key.strip().lower()
        if not sep or not value.strip():
            raise ValueError(f"kernel limit {entry!r}: expected KEY=VALUE")
        if key not in LIMIT_KEYS:
            raise ValueError(f"unknown kernel limit {key!r}; known: {', '.join(LIMIT_KEYS)}")
        env[f"LAZYOS_LIMIT_{key.upper()}"] = value.strip()
    return env


def add_limit_option(parser) -> None:
    """`--limit KEY=VALUE` (repeatable) on an argparse parser."""
    parser.add_argument("--limit", action="append", default=[], metavar="KEY=VALUE",
                        help="kernel limit written to lazyos.cfg (LAZYOS_LIMIT_<KEY>); "
                             f"repeatable; keys: {', '.join(LIMIT_KEYS)}")


def build_limits(entries, no_build: bool) -> dict[str, str]:
    """:func:`limit_env`, refusing limits a skipped build could never apply."""
    env = limit_env(entries)
    if env and no_build:
        raise ValueError("--limit needs a build: the limits are written into lazyos.cfg")
    return env
