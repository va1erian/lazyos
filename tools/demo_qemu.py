"""QEMU arguments `tools/run_demo.py` derives from its flags."""

from __future__ import annotations

import sys
from pathlib import Path


def sound_args(backend: str) -> list[str]:
    """QEMU arguments for a virtio-sound card on `backend` (see `--sound`)."""
    if backend == "auto":
        backend = {"win32": "dsound", "darwin": "coreaudio"}.get(sys.platform, "pa")
    if backend.startswith("wav:"):
        # A comma in a path is doubled for QEMU's option parser.
        path = Path(backend[4:]).resolve().as_posix().replace(",", ",,")
        audiodev = f"wav,id=snd0,path={path}"
    else:
        audiodev = f"{backend},id=snd0"
    return ["-audiodev", audiodev, "-device", "virtio-sound-pci,audiodev=snd0"]
