#!/usr/bin/env python3
"""LazyOS USB stick maker: build the stick image and write it, from a window.

    python3 tools/boot/stick_gui.py       # Linux: as your user (pkexec asks for root to write)
    python tools\\boot\\stick_gui.py       # Windows: in an Administrator prompt

Three steps (docs/usb-stick.md):

1. **Image**: pick `target/lazyos-usb.img`, or build it (the desktop profile,
   `run_demo.py --desktop --usb-image --build-only`, which also builds the
   xui apps and BusyBox when they are missing).
2. **Stick**: pick a removable or USB disk; the ones `write_stick.py` refuses
   are listed with the reason and cannot be picked.
3. **Write**: two confirmations (the second asks for the device name typed
   back), then `write_stick.py --yes` writes the image and reads it back to
   compare SHA-256 digests. On Linux it runs through `pkexec` (or the GUI's
   own root), so the build never runs as root.

The checks, the write and the verification are `write_stick.py`'s; this window
only drives it (Tkinter, standard library only).
"""

from __future__ import annotations

import os
import re
import shutil
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(ROOT / "tools"))

import write_stick  # noqa: E402

DEFAULT_IMAGE = ROOT / "target" / "lazyos-usb.img"
HOME_SIZES = ["1G", "4G", "8G", "16G", "28G"]
PROGRESS = re.compile(r"(wrote|verified) (\d+) / (\d+) MiB")


# ----- Logic (no Tk: tools/boot/test_stick_gui.py covers it) -------------

def build_step(release: bool) -> dict:
    """The build as a `Runner` step: the desktop stick image, nothing booted."""
    argv = [sys.executable, "-u", str(ROOT / "tools" / "run_demo.py"),
            "--desktop", "--usb-image", "--build-only"]
    if release:
        argv.append("--release")
    return {"label": "build the stick image", "argv": argv}


def build_env(home_size: str) -> dict[str, str]:
    """The build switches (the runner drops every inherited LAZYOS_*)."""
    return {"LAZYOS_USB_HOME_SIZE": home_size}


def is_admin() -> bool:
    """Whether this process may open a raw disk for writing."""
    if os.name == "nt":
        try:
            import ctypes
            return bool(ctypes.windll.shell32.IsUserAnAdmin())
        except Exception:
            return False
    return os.geteuid() == 0


def elevation() -> list[str] | None:
    """The prefix that runs the writer with raw-disk rights, or None if there is none."""
    if is_admin():
        return []
    if os.name != "nt" and shutil.which("pkexec"):
        return ["pkexec"]
    return None


def write_step(prefix: list[str], image: Path, device: str) -> dict:
    """`write_stick.py --yes` as a `Runner` step; absolute paths (pkexec resets cwd and env)."""
    argv = prefix + [sys.executable, "-u", str(HERE / "write_stick.py"),
                     "--image", str(image.resolve()), "--device", device, "--yes"]
    return {"label": f"write {device}", "argv": argv}


def progress(line: str) -> tuple[str, int, int] | None:
    """`("wrote"|"verified", done MiB, total MiB)` from a writer output line."""
    match = PROGRESS.search(line)
    if not match:
        return None
    return match.group(1), int(match.group(2)), int(match.group(3))


def image_summary(image: Path) -> str:
    """One line about the image file, or what to do when it is missing."""
    if not image.is_file():
        return "not built yet: press Build image"
    size = image.stat().st_size
    built = time.strftime("%Y-%m-%d %H:%M", time.localtime(image.stat().st_mtime))
    return f"{size >> 20} MiB, built {built}"


def disk_rows(disks: list[write_stick.Disk], image: Path) -> list[tuple[write_stick.Disk, str | None]]:
    """Each disk with `write_stick.refusal`'s reason (None when it may be written)."""
    size = image.stat().st_size if image.is_file() else 0
    return [(disk, write_stick.refusal(disk, size)) for disk in disks]


# ----- The window --------------------------------------------------------

def main() -> int:
    import tkinter as tk
    from tkinter import filedialog, messagebox, simpledialog, ttk

    from lazygui.runner import Runner

    root = tk.Tk()
    root.title("LazyOS USB stick maker")
    root.minsize(640, 520)
    runner = Runner()
    state = {"mode": None, "rows": []}

    image_var = tk.StringVar(value=str(DEFAULT_IMAGE))
    image_info = tk.StringVar()
    release_var = tk.BooleanVar(value=True)
    home_var = tk.StringVar(value="1G")
    status_var = tk.StringVar(value="Ready.")

    outer = ttk.Frame(root, padding=10)
    outer.pack(fill="both", expand=True)

    # 1. Image
    box1 = ttk.LabelFrame(outer, text="1. Image", padding=8)
    box1.pack(fill="x")
    row = ttk.Frame(box1)
    row.pack(fill="x")
    ttk.Entry(row, textvariable=image_var).pack(side="left", fill="x", expand=True)

    def browse() -> None:
        path = filedialog.askopenfilename(initialdir=str(Path(image_var.get()).parent),
                                          filetypes=[("Disk images", "*.img"), ("All", "*")])
        if path:
            image_var.set(path)

    ttk.Button(row, text="Browse…", command=browse).pack(side="left", padx=(6, 0))
    ttk.Label(box1, textvariable=image_info).pack(anchor="w", pady=(4, 4))
    opts = ttk.Frame(box1)
    opts.pack(fill="x")
    ttk.Checkbutton(opts, text="Release build (for real hardware)",
                    variable=release_var).pack(side="left")
    ttk.Label(opts, text="   /home size:").pack(side="left")
    ttk.Combobox(opts, textvariable=home_var, values=HOME_SIZES, width=6).pack(side="left")
    build_btn = ttk.Button(opts, text="Build image")
    build_btn.pack(side="right")

    # 2. Stick
    box2 = ttk.LabelFrame(outer, text="2. Stick (everything on it will be erased)", padding=8)
    box2.pack(fill="x", pady=(8, 0))
    tree = ttk.Treeview(box2, columns=("dev", "size", "model", "note"), show="headings",
                        height=4, selectmode="browse")
    for col, title, width in (("dev", "Device", 150), ("size", "Size", 80),
                              ("model", "Model", 180), ("note", "", 220)):
        tree.heading(col, text=title)
        tree.column(col, width=width, stretch=col in ("model", "note"))
    tree.tag_configure("refused", foreground="gray")
    tree.pack(fill="x")
    refresh_btn = ttk.Button(box2, text="Refresh")
    refresh_btn.pack(anchor="e", pady=(4, 0))

    # 3. Write
    box3 = ttk.LabelFrame(outer, text="3. Write", padding=8)
    box3.pack(fill="x", pady=(8, 0))
    bar = ttk.Progressbar(box3, maximum=200)
    bar.pack(fill="x")
    line = ttk.Frame(box3)
    line.pack(fill="x", pady=(4, 0))
    ttk.Label(line, textvariable=status_var).pack(side="left")
    write_btn = ttk.Button(line, text="Write to stick")
    write_btn.pack(side="right")
    stop_btn = ttk.Button(line, text="Stop", state="disabled")
    stop_btn.pack(side="right", padx=(0, 6))

    log = tk.Text(outer, height=10, wrap="char", state="disabled")
    log.pack(fill="both", expand=True, pady=(8, 0))

    def say(text: str) -> None:
        log.configure(state="normal")
        log.insert("end", text)
        log.see("end")
        log.configure(state="disabled")

    def refresh_image(*_: object) -> None:
        image_info.set(image_summary(Path(image_var.get())))

    def refresh_disks() -> None:
        tree.delete(*tree.get_children())
        try:
            disks = write_stick.list_disks()
        except Exception as exc:  # e.g. PowerShell missing
            status_var.set(f"could not list disks: {exc}")
            disks = []
        state["rows"] = disk_rows(disks, Path(image_var.get()))
        for index, (disk, reason) in enumerate(state["rows"]):
            tree.insert("", "end", iid=str(index), tags=("refused",) if reason else (),
                        values=(disk.path, f"{disk.size / 1e9:.1f} GB", disk.model or "?",
                                f"refused: {reason}" if reason else ("USB" if disk.usb else "removable")))
        if not state["rows"]:
            tree.insert("", "end", iid="none", values=("", "", "no removable or USB disk found", ""),
                        tags=("refused",))

    def selected() -> tuple[write_stick.Disk, str | None] | None:
        pick = tree.selection()
        if not pick or pick[0] == "none":
            return None
        return state["rows"][int(pick[0])]

    def busy(on: bool) -> None:
        for widget in (build_btn, write_btn, refresh_btn):
            widget.configure(state="disabled" if on else "normal")
        # A stopped write leaves the stick unusable, and a root writer cannot
        # be signalled from here anyway: only a build can be stopped.
        stop_btn.configure(state="normal" if on and state["mode"] == "build" else "disabled")

    def start(mode: str, step: dict, env: dict[str, str]) -> None:
        state["mode"] = mode
        bar["value"] = 0
        busy(True)
        runner.start([step], env, str(ROOT))

    def on_build() -> None:
        say("\n")
        status_var.set("Building… (the first build takes a while)")
        start("build", build_step(release_var.get()), build_env(home_var.get().strip() or "1G"))

    def on_write() -> None:
        image = Path(image_var.get())
        if not image.is_file():
            messagebox.showerror("No image", f"{image} does not exist. Build it first.")
            return
        pick = selected()
        if pick is None:
            messagebox.showinfo("Pick a stick", "Select the USB stick to write in the list.")
            return
        disk, reason = pick
        if reason:
            messagebox.showerror("Refused", f"{disk.path} cannot be written: {reason}.")
            return
        prefix = elevation()
        if prefix is None:
            hint = ("Run this window from an Administrator prompt." if os.name == "nt"
                    else "Install pkexec (polkit) or run this window with sudo.")
            messagebox.showerror("Needs administrator rights", hint)
            return
        offline = (f"\n\nIts volumes ({', '.join(disk.mounted)}) are dismounted: the disk goes "
                   "offline for the write." if os.name == "nt" and disk.mounted else "")
        if not messagebox.askyesno(
                "Erase this disk?",
                f"Write {image.name} ({image.stat().st_size >> 20} MiB) to:\n\n"
                f"{disk.describe()}\n\nEVERYTHING on this disk will be destroyed.{offline}",
                icon="warning", default="no"):
            return
        typed = simpledialog.askstring("Confirm",
                                       f"Type the device name ({disk.path}) to confirm:",
                                       parent=root)
        if (typed or "").strip() != disk.path:
            status_var.set("Nothing written (the device name did not match).")
            return
        say("\n")
        status_var.set(f"Writing {disk.path}…")
        start("write", write_step(prefix, image, disk.path), {})

    def poll() -> None:
        while not runner.q.empty():
            kind, *rest = runner.q.get_nowait()
            if kind == "step":
                say(f"$ {rest[1]}\n")
            elif kind == "out":
                found = progress(rest[0])
                if not found:
                    say(rest[0])  # the progress counters go to the bar, not the log
                elif found[2]:
                    phase, done, total = found
                    bar["value"] = (100 if phase == "verified" else 0) + 100 * done / total
                    status_var.set(f"{'Verifying' if phase == 'verified' else 'Writing'} "
                                   f"{done} / {total} MiB")
            elif kind == "done":
                code = rest[0]
                mode = state["mode"]
                busy(False)
                refresh_image()
                refresh_disks()
                if mode == "build":
                    status_var.set("Image built." if code == 0 else "Build failed: see the log.")
                elif code == 0:
                    bar["value"] = 200
                    status_var.set("Done: the stick is written and verified.")
                    messagebox.showinfo(
                        "Stick ready",
                        "Written and verified.\n\nPlug the stick into the PC, turn Secure Boot "
                        "and Fast Boot off, and pick it in the firmware's boot menu "
                        "(F8 on ASUS boards). See docs/usb-stick.md.")
                else:
                    status_var.set("Write failed: see the log.")
        root.after(100, poll)

    def on_stop() -> None:
        runner.stop()
        status_var.set("Stopping…")

    def on_close() -> None:
        if runner.busy and state["mode"] == "write" and not messagebox.askyesno(
                "Write in progress", "Stopping now leaves the stick unusable. Quit anyway?"):
            return
        runner.stop()
        root.destroy()

    build_btn.configure(command=on_build)
    write_btn.configure(command=on_write)
    refresh_btn.configure(command=refresh_disks)
    stop_btn.configure(command=on_stop)
    image_var.trace_add("write", refresh_image)
    root.protocol("WM_DELETE_WINDOW", on_close)
    refresh_image()
    refresh_disks()
    if elevation() is None:
        say("note: writing needs administrator rights "
            + ("(start this from an Administrator prompt).\n" if os.name == "nt"
               else "(install pkexec, or run with sudo).\n"))
    root.after(100, poll)
    root.mainloop()
    return 0


if __name__ == "__main__":
    sys.exit(main())
