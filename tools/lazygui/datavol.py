"""The "Home volume" group: show the persistent ext2 home disk, reset it, toggle it.

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


def seed_summary() -> str:
    """One line saying what a reset creates (``/admin``, ``/user``).

    Shown before the click so nobody discovers the layout by surprise; the
    accounts come from ``build_support/passwd``, the same file the formatter reads.
    """
    try:
        plan = mkdisk.home_volume()
    except (OSError, ValueError) as exc:  # e.g. build_support/passwd is missing
        return f"Reset layout unavailable: {exc}"
    dirs = ", ".join(f"{spec.path} ({spec.mode:o})" for spec in plan.dirs)
    return (f"Reset creates an empty volume (label {mkdisk.HOME_LABEL}, mounted at /home) "
            f"with: {dirs}. Existing data is erased.")


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
    ttk.Label(parent, text=seed_summary(), foreground="#444",
              wraplength=440).pack(fill="x", padx=6)
    row = ttk.Frame(parent)
    row.pack(fill="x", padx=6, pady=(2, 6))
    ttk.Checkbutton(row, text="Attach to Interactive demo",
                    variable=attach_var).pack(side="left")
    ttk.Button(row, text="Reset volume", command=on_reset).pack(side="right")
    return label


def reset(path: str, busy: bool) -> tuple[bool, str] | None:
    """Confirm, then format a freshly seeded volume at ``path``.

    Returns ``(succeeded, log line)``, or ``None`` if the user declined.
    Refused while a run is active because QEMU may hold the file.
    """
    if busy:
        return False, "Cannot reset the home volume while a run is active; stop it first."
    target = Path(path)
    try:
        plan = mkdisk.home_volume()
    except (OSError, ValueError) as exc:
        return False, f"Reset failed: {exc}"
    if target.exists() and not messagebox.askyesno(
            "Reset home volume",
            f"Erase everything on {target} and format a fresh volume containing:\n\n"
            f"{mkdisk.describe(plan)}\n\nThis cannot be undone."):
        return None
    try:
        mkdisk.format_image(target, label=mkdisk.HOME_LABEL, layout=plan)
    except (OSError, ValueError) as exc:  # e.g. QEMU still has the file open
        return False, f"Reset failed: {exc}"
    return True, f"Home volume reset: {mkdisk.status(target).describe()}"


def confirm_reset_os(cfg: dict) -> bool:
    """Ask before a run that recreates the OS volume; ``True`` when the run may go on.

    Mirrors the plan rule in ``catalog.build_plan``: the flag only takes effect
    in the interactive demo, and not with "Skip build".
    """
    if not ((cfg.get("reset_os") or cfg.get("setup")) and not cfg.get("skip_build")
            and cfg.get("mode") == "Interactive demo"):
        return True
    return messagebox.askyesno(
        "Recreate the OS volume",
        "Erase the OS volume in target/lazyos.img and rebuild it?\n\n"
        "Installed apps, settings, logs and /data are lost. This cannot be undone.")
