"""QEMU volume options shared by the launchers: the ``/home`` and data disks
(virtio-blk, after the boot disk) and their ``--home-disk``/``--data-disk``
command-line options. Split out of ``qemu_qmp.py``, which re-exports them.
"""

from __future__ import annotations

from pathlib import Path


def data_disk_args(path: str | Path) -> list[str]:
    """QEMU arguments attaching the persistent data volume as virtio-blk.

    It is always a *second*, separate device from the boot disk. QEMU's option
    parser treats a comma as a separator, so a literal one in the path is
    doubled.
    """
    file = Path(path).resolve().as_posix().replace(",", ",,")
    return ["-drive", f"format=raw,file={file},if=none,id=data",
            "-device", "virtio-blk-pci,drive=data"]


def home_disk_args(path: str | Path) -> list[str]:
    """QEMU arguments attaching the home volume (``target/home.img``) as virtio-blk.

    Attached after the boot disk and after any data disk, so the order the
    kernel enumerates virtio devices in (PCI order) is deterministic. The kernel
    finds the volume by its ``lazyhome`` label, not by position.
    """
    file = Path(path).resolve().as_posix().replace(",", ",,")
    return ["-drive", f"format=raw,file={file},if=none,id=home",
            "-device", "virtio-blk-pci,drive=home"]


def add_home_disk_option(parser) -> None:
    """Add ``--home-disk PATH`` (off by default so CI runs stay hermetic)."""
    parser.add_argument("--home-disk", metavar="PATH",
                        help="attach this existing home volume as a virtio-blk device after "
                             "the boot disk (create one with "
                             "`python -m tools.mkdisk PATH --home-volume`)")


def existing_home_disk(value: str | None) -> Path | None:
    """The ``--home-disk`` file, or exit with a hint if it does not exist."""
    if not value:
        return None
    path = Path(value).resolve()
    if not path.is_file():
        raise SystemExit(f"--home-disk not found: {path}\n"
                         f"Create it with: python -m tools.mkdisk {value} --home-volume")
    return path


def add_data_disk_option(parser) -> None:
    """Add ``--data-disk PATH`` (off by default so CI runs stay hermetic)."""
    parser.add_argument("--data-disk", metavar="PATH",
                        help="attach this existing ext2 volume as a second virtio-blk "
                             "device (create one with `python -m tools.mkdisk PATH`)")


def existing_data_disk(value: str | None) -> Path | None:
    """The ``--data-disk`` file, or exit with a hint if it does not exist.

    The scripted tools never create it: a missing volume is more likely a typo
    than a request to format a new one.
    """
    if not value:
        return None
    path = Path(value).resolve()
    if not path.is_file():
        raise SystemExit(f"--data-disk not found: {path}\n"
                         f"Create it with: python -m tools.mkdisk {value}")
    return path
