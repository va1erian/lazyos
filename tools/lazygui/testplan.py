"""Plan helpers shared by the launcher tests (`test_catalog*.py`)."""

from __future__ import annotations

from pathlib import Path

from lazygui import catalog, osvolume

# The plans must not depend on the developer's real target/lazyos.img (its OS
# volume size decides whether a build needs a reset): point both at no image.
_NO_IMAGE = Path(__file__).resolve().parent / "no-such-lazyos.img"
catalog.IMAGE = str(_NO_IMAGE)
osvolume.DEFAULT_IMAGE = _NO_IMAGE


def demo_config(**overrides) -> dict:
    """A minimal Interactive-demo configuration."""
    cfg = {"mode": "Interactive demo", "profile": "dev", "skip_build": True,
           "headless": False, "accel": "auto", "memory": "1G", "qemu": "", "extra": ""}
    cfg.update(overrides)
    return cfg


def demo_argv(**overrides) -> list[str]:
    return catalog.build_plan(demo_config(**overrides))[-1]["argv"]
