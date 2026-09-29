"""The "Data volume" group: show the persistent ext2 disk, reset it, toggle it.

Formatting happens in-process through :mod:`mkdisk` (the same code behind
``run_demo.py --reset-data``), never by shelling out: Windows has no
``mkfs.ext2``.
"""

from __future__ import annotations

from pathlib import Path
from tkinter import messagebox, ttk
from typing import Callable

import mkdisk


def status_text(path: str) -> str:
    """Path, size and existence of the volume for the label under the entry."""
    return mkdisk.status(Path(path)).describe()


def build_group(parent: ttk.Frame, path_var, attach_var, on_reset: Callable[[], None]) -> ttk.Label:
    """Populate ``parent`` with the volume controls; return the status label.

    ``path_var`` / ``attach_var`` are the Tk variables the launch plan reads;
    the caller refreshes the returned label whenever ``path_var`` changes.
    """
    row = ttk.Frame(parent)
    row.pack(fill="x", padx=6, pady=2)
    ttk.Label(row, text="Path:", width=12).pack(side="left")
    ttk.Entry(row, textvariable=path_var).pack(side="left", fill="x", expand=True)
    label = ttk.Label(parent, foreground="#444", wraplength=440)
    label.pack(fill="x", padx=6)
    row = ttk.Frame(parent)
    row.pack(fill="x", padx=6, pady=(2, 6))
    ttk.Checkbutton(row, text="Attach to Interactive demo",
                    variable=attach_var).pack(side="left")
    ttk.Button(row, text="Reset volume", command=on_reset).pack(side="right")
    return label


def reset(path: str, busy: bool) -> tuple[bool, str] | None:
    """Confirm, then format an empty volume at ``path``.

    Returns ``(succeeded, log line)``, or ``None`` if the user declined.
    Refused while a run is active because QEMU may hold the file.
    """
    if busy:
        return False, "Cannot reset the data volume while a run is active; stop it first."
    target = Path(path)
    if target.exists() and not messagebox.askyesno(
            "Reset data volume",
            f"Erase everything on {target} and format an empty volume?\n\nThis cannot be undone."):
        return None
    try:
        mkdisk.format_image(target)
    except (OSError, ValueError) as exc:  # e.g. QEMU still has the file open
        return False, f"Reset failed: {exc}"
    return True, f"Data volume reset: {mkdisk.status(target).describe()}"
