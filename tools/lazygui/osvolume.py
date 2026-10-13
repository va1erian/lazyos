"""Size the OS volume for what an image carries (`LAZYOS_OS_SIZE`).

The build refuses to resize an existing OS volume, so a selection that needs
more room than `target/lazyos.img` has (SRB2Kart's game data) used to fail at
the very last build step. This works the size out first, shared by the GUI
(`catalog.py`) and `tools/run_demo.py`:

* a new image gets the size the selection needs;
* an existing image that is big enough keeps its size (so switching a big
  feature off never trips the "resizing is not supported" refusal);
* an existing image that is too small has to be recreated (``--reset-os``).

The sizes mirror `build_support/os_disk.rs` (`DEFAULT_OS_SIZE`,
`SRB2KART_OS_SIZE`, `WEBKIT_OS_SIZE`, `OS_START_LBA`).
"""

from __future__ import annotations

import os
from pathlib import Path

#: The image every build writes (`build.rs`'s `target/lazyos.img`).
DEFAULT_IMAGE = Path(__file__).resolve().parents[2] / "target" / "lazyos.img"
MIB = 1 << 20
SECTOR = 512
#: Where the OS volume starts in the MBR disk (`os_disk::OS_START_LBA`).
OS_START_LBA = 131_072
#: `os_disk::DEFAULT_OS_SIZE`, `SRB2KART_OS_SIZE` and `WEBKIT_OS_SIZE`.
DEFAULT_SIZE = 512 * MIB
SRB2KART_SIZE = 1024 * MIB
WEBKIT_SIZE = 1024 * MIB

_TABLE = 0x1BE
_ENTRY = 16


def required_bytes(srb2kart: bool, webkit: bool = False) -> int:
    """The OS volume size a selection needs when none is configured (the largest of
    what each selected feature needs, as `os_disk::default_size`)."""
    if srb2kart:
        return SRB2KART_SIZE
    return WEBKIT_SIZE if webkit else DEFAULT_SIZE


def existing_bytes(image: Path) -> int | None:
    """The OS volume size of an existing disk image, ``None`` when there is no
    readable one (the build then creates it)."""
    try:
        with Path(image).open("rb") as handle:
            mbr = handle.read(512)
    except OSError:
        return None
    if len(mbr) < 512 or mbr[510:512] != b"\x55\xaa":
        return None
    for index in range(4):
        entry = mbr[_TABLE + index * _ENTRY:_TABLE + (index + 1) * _ENTRY]
        start = int.from_bytes(entry[8:12], "little")
        sectors = int.from_bytes(entry[12:16], "little")
        if start == OS_START_LBA and sectors:
            return sectors * SECTOR
    return None


def plan(image: Path, srb2kart: bool, webkit: bool = False) -> tuple[int | None, bool]:
    """``(size to ask for, whether the volume must be recreated)``.

    The size is ``None`` when the default is right (no image yet, or one of
    exactly the needed size).
    """
    need = required_bytes(srb2kart, webkit)
    have = existing_bytes(image)
    if have is None or have == need:
        return None, False
    if have > need:
        return have, False
    return need, True


def too_small(image: Path, srb2kart: bool, webkit: bool = False) -> tuple[int, int] | None:
    """``(have MiB, need MiB)`` when ``image``'s OS volume cannot hold the
    selection and must be recreated, else ``None``."""
    _size, reset = plan(image, srb2kart, webkit)
    if not reset:
        return None
    return (existing_bytes(image) or 0) // MIB, required_bytes(srb2kart, webkit) // MIB


def configured() -> bool:
    """Whether the caller already chose a size (`LAZYOS_OS_SIZE`): theirs wins."""
    return bool(os.environ.get("LAZYOS_OS_SIZE", "").strip())


def wants_webkit(cfg: dict) -> bool:
    """Whether a launcher configuration selects the browser or the console proof: the same three
    programs of about 107 MiB each."""
    return bool(cfg.get("webkit") or cfg.get("webkit_port") or cfg.get("lazywebplus"))


def cfg_too_small(cfg: dict, image: str | Path) -> tuple[int, int] | None:
    """:func:`too_small` for a launcher configuration: ``(have MiB, need MiB)``
    when the existing image cannot hold the selection, else ``None``."""
    if configured() or cfg.get("skip_build"):
        return None
    return too_small(Path(image), bool(cfg.get("srb2kart")), wants_webkit(cfg))


def check(cfg: dict, image: str | Path) -> None:
    """Refuse, before a long build, a non-demo run that would only fail at the
    last step on a too-small OS volume (``ValueError``, shown as a plan error).
    The interactive demo recreates the volume itself, asking first."""
    small = cfg_too_small(cfg, image)
    if small and cfg.get("mode") != "Interactive demo" and not cfg.get("reset_os"):
        raise ValueError(
            f"the OS volume in target/lazyos.img is {small[0]} MiB but this selection needs "
            f"{small[1]} MiB (a volume cannot be resized): tick \"Recreate the OS volume\" "
            "(it erases installed apps, settings, logs and /data) or turn the large "
            "feature off")


def env(cfg: dict, image: str | Path) -> dict[str, str]:
    """`LAZYOS_OS_SIZE` for a build: what the selection needs, or the size an
    existing big-enough volume already has; nothing when the default is right
    or the caller set one."""
    if configured():
        return {}
    size, _reset = plan(Path(image), bool(cfg.get("srb2kart")), wants_webkit(cfg))
    return {"LAZYOS_OS_SIZE": str(size)} if size else {}


def reset_wanted(cfg: dict, image: str | Path) -> bool:
    """Whether the interactive demo recreates the OS volume: asked for, a
    first-boot setup, or an existing one too small for the selection."""
    return bool(cfg.get("reset_os") or cfg.get("setup") or cfg_too_small(cfg, image))
