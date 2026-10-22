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
`SRB2KART_EXTRA`, `WEBKIT_EXTRA`, `OS_START_LBA`): the default plus what each
selected feature adds, since SRB2Kart's data beside the browser's programs does
not fit the volume either one alone needs.
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
#: `os_disk::DEFAULT_OS_SIZE`, `SRB2KART_EXTRA` and `WEBKIT_EXTRA`.
DEFAULT_SIZE = 512 * MIB
SRB2KART_EXTRA = 512 * MIB
WEBKIT_EXTRA = 512 * MIB
#: One feature alone (`SRB2KART_OS_SIZE`, `WEBKIT_OS_SIZE`).
SRB2KART_SIZE = DEFAULT_SIZE + SRB2KART_EXTRA
WEBKIT_SIZE = DEFAULT_SIZE + WEBKIT_EXTRA

_TABLE = 0x1BE
_ENTRY = 16


def required_bytes(srb2kart: bool, webkit: bool = False) -> int:
    """The OS volume size a selection needs when none is configured (the default
    plus what each selected feature adds, as `os_disk::default_size`)."""
    return (DEFAULT_SIZE + (SRB2KART_EXTRA if srb2kart else 0)
            + (WEBKIT_EXTRA if webkit else 0))


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


#: The configurations whose image is the desktop (`catalog.build_env` sets `LAZYOS_DESKTOP`).
DESKTOP_KEYS = ("desktop", "lazysky", "lazywebplus", "webd", "webd_demo", "setup")


def leaves_webd_out(cfg: dict) -> bool:
    """Whether the launcher leaves webd out of the image (`no_webd`): never with LazyWeb+, which is a client
    of webd and refuses `--no-webd` (the option is ignored there)."""
    return bool(cfg.get("no_webd") and not cfg.get("lazywebplus"))


def wants_webd(cfg: dict) -> bool:
    """Whether the image ships webd: every desktop does (Docs and Mail draw their pages with it) unless the
    launcher left it out, and `webd` / `webd_demo` ask for it outright."""
    if leaves_webd_out(cfg):
        return False
    return bool(cfg.get("webd") or cfg.get("webd_demo") or any(cfg.get(key) for key in DESKTOP_KEYS))


def requires_webd(cfg: dict) -> bool:
    """Whether the launcher asks for webd outright (`webd`, `webd_demo`): built, and a failure stops the
    build. A plain desktop only ships it when it has been built (`LAZYOS_WEBD` unset, no Docker step)."""
    return bool(not leaves_webd_out(cfg) and (cfg.get("webd") or cfg.get("webd_demo")))


def wants_webkit(cfg: dict) -> bool:
    """Whether a launcher configuration selects the browser, webd or the console proof: the same three
    programs of about 107 MiB each."""
    return bool(cfg.get("webkit_port") or cfg.get("lazywebplus") or wants_webd(cfg))


def args_wants(args) -> tuple[bool, bool]:
    """``(srb2kart, webkit)`` for `run_demo.py`'s parsed flags, or the same
    switches already in the environment."""
    def on(flag: str, switch: str) -> bool:
        return bool(getattr(args, flag, False)) or os.environ.get(switch) == "1"
    return (on("srb2kart", "LAZYOS_SRB2KART"),
            on("webkit_port", "LAZYOS_WEBKIT_PORT") or on("lazywebplus", "LAZYOS_LAZYWEBPLUS")
            or on("webd", "LAZYOS_WEBD") or bool(getattr(args, "webd_implicit", False)))


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
    """Whether a build recreates the OS volume: asked for, a first-boot setup,
    or an existing one too small for the selection."""
    return bool(cfg.get("reset_os") or cfg.get("setup") or cfg_too_small(cfg, image))


#: The run modes that build `target/lazyos.img` (unless "Skip build" is ticked).
BUILDING_MODES = ("Interactive demo", "Headless screenshots", "Scripted session",
                  "Kernel test suite")


def reset_on_run(cfg: dict, image: str | Path) -> bool:
    """Whether running ``cfg`` recreates the OS volume: it builds the image
    (its mode does, and "Skip build" is off) and :func:`reset_wanted`."""
    return (cfg.get("mode") in BUILDING_MODES and not cfg.get("skip_build")
            and reset_wanted(cfg, image))


def reset_on_build_button(cfg: dict, image: str | Path) -> bool:
    """The same for the "Build image" button, which builds whatever the mode
    and "Skip build" say: a volume too small for the selection is recreated,
    after the launcher has asked, since a build cannot resize it."""
    return reset_wanted({**cfg, "skip_build": False}, image)
