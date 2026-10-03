"""Kernel limits as image build switches (`LAZYOS_LIMIT_*` -> `limit.*` in `lazyos.cfg`).

Shared by the GUI (`catalog.py`) and `tools/run_demo.py`; the keys mirror
`kernel/src/limits.rs` (docs/architecture/limits.md) and the image build
validates the values themselves (`build_support/limits_cfg.rs`).
"""

from __future__ import annotations

#: The kernel limits `lazyos.cfg` can set; `KEY=VALUE` becomes `LAZYOS_LIMIT_<KEY>`.
LIMIT_KEYS = ("heap_max", "fd_max", "stack_size", "quota_user_memory",
              "quota_kernel_memory", "shared_buffer_max")


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
