"""The Tkinter window: configuration controls, plan preview, and output log."""

from __future__ import annotations

import os
import queue
import tkinter as tk
from tkinter import filedialog, ttk

from . import datavol, driveropts, netopts
from .catalog import (ACCELS, CARGO, DISKS, MODES, PY, ROOT, SCRIPTS, XUI_VIEWERS, build_env,
                      build_plan, format_plan, image_build, simple_config)
from .runner import Runner, open_path
from .scriptenv import SCRIPT_ENV
from .scroll import scrollable
from .simple import SIMPLE_EXTRAS, build_simple_tab, simple_choice
from .variables import make_vars


class Launcher:
    """The whole GUI: widgets, bound config variables, and the run loop."""

    def __init__(self, root: tk.Tk) -> None:
        """Build the window on ``root`` and start the output poll timer."""
        self.root = root
        root.title("LazyOS Launcher")
        root.geometry("1020x760")
        root.minsize(880, 620)
        self.runner = Runner()
        self.v = make_vars()
        self._build()
        self._bind()
        self._on_mode()
        self._refresh_volume()
        self._log("LazyOS Launcher ready.\n", "ok")
        self._log(f"root:   {ROOT}\npython: {PY}\ncargo:  {CARGO}\n", "dim")
        self._log("Simple: pick Debug/Release and CLI/Desktop, then Start. "
                  "Advanced: pick a mode, set the configuration, then press Run.\n", "dim")

    # ---------------------------------------------------------------- state
    def cfg(self) -> dict:
        """The configuration for the active tab (Simple choices or Advanced controls)."""
        if self.notebook.select() == str(self.tab_simple):
            profile, iface = simple_choice(self.v["simple_build"].get(),
                                           self.v["simple_iface"].get())
            return simple_config(self._advanced_cfg(), profile, iface,
                                 *(self.v[f"simple_{name}"].get() for name in SIMPLE_EXTRAS))
        return self._advanced_cfg()

    def _advanced_cfg(self) -> dict:
        """Snapshot the Advanced controls into the dict catalog.build_plan expects."""
        names = [n for n in SCRIPTS if n[1] == self.v["script"].get()]
        return {
            "mode": self.v["mode"].get(),
            "profile": self.v["profile"].get(),
            "accel": self.v["accel"].get(),
            "disk": self.v["disk"].get(),
            "home_path": self.v["home_path"].get().strip(),
            "home_disk": self.v["home_disk"].get(),
            "data_path": self.v["data_path"].get().strip(),
            "data_disk": self.v["data_disk"].get(),
            "reset_os": self.v["reset_os"].get(),
            "memory": self.v["memory"].get().strip(),
            "limits": self.v["limits"].get().strip(),
            "display_mode": self.v["display_mode"].get().strip(),
            "assets": self.v["assets"].get().strip(),
            "times": self.v["times"].get().strip(),
            "timeout": self.v["timeout"].get().strip(),
            "abi_time": self.v["abi_time"].get().strip(),
            "abi_only": self.v["abi_only"].get().strip(),
            "extra": self.v["extra"].get().strip(),
            "out": self.v["out"].get().strip(),
            "qemu": self.v["qemu"].get().strip(),
            "busybox": self.v["busybox"].get().strip(),
            "skip_build": self.v["skip_build"].get(),
            "headless": self.v["headless"].get(),
            "tablet": self.v["tablet"].get(),
            "sound": self.v["sound"].get(),
            "sound_card": self.v["sound_card"].get(),
            "nic": self.v["nic"].get(),
            "devd": self.v["devd"].get(),
            "abi_build": self.v["abi_build"].get(),
            "desktop": self.v["desktop"].get(),
            "services": self.v["services"].get(),
            "xuid": self.v["xuid"].get(),
            "shellprobe": self.v["shellprobe"].get(),
            "msgctl": self.v["msgctl"].get(),
            "msgrd": self.v["msgrd"].get(),
            "xui_client": self.v["xui_client"].get(),
            "xui_app": self.v["xui_app"].get(),
            "xui_autostart": self.v["xui_autostart"].get(),
            "lazyrad": self.v["lazyrad"].get(),
            "usb_image": self.v["usb_image"].get(),
            "shell": self.v["shell"].get(),
            "lazyrad_samples": self.v["lazyrad_samples"].get().strip(),
            "devices": self.v["devices"].get(),
            "doom": self.v["doom"].get(),
            "modplayer": self.v["modplayer"].get(),
            "net": self.v["net"].get(),
            "net_forwards": self.v["net_forwards"].get().strip(),
            "net_restrict": self.v["net_restrict"].get(),
            "linuxapps": self.v["linuxapps"].get(),
            # Mail speaks TLS: its switch brings the HTTPS stack and the card.
            "tls": self.v["tls"].get() or self.v["mail"].get(),
            "journal": self.v["journal"].get(),
            "lazyweb": self.v["lazyweb"].get(),
            "mail": self.v["mail"].get(),
            "traydemo": self.v["traydemo"].get(),
            "script": SCRIPTS.index(names[0]) if names else 0,
        }

    # ------------------------------------------------------------------ ui
    def _build(self) -> None:
        """Assemble the bottom bar and the scrollable-left / log-right split."""
        style = ttk.Style()
        if "clam" in style.theme_names():
            style.theme_use("clam")
        self._build_bottom()
        split = ttk.Panedwindow(self.root, orient="horizontal")
        split.pack(side="top", fill="both", expand=True, padx=4, pady=(4, 0))
        left = ttk.Frame(split)
        right = ttk.Frame(split)
        split.add(left, weight=0)
        split.add(right, weight=1)
        self.notebook = ttk.Notebook(left)
        self.notebook.pack(fill="both", expand=True)
        self.tab_simple = ttk.Frame(self.notebook)
        tab_adv = ttk.Frame(self.notebook)
        self.notebook.add(self.tab_simple, text="Simple")
        self.notebook.add(tab_adv, text="Advanced")
        build_simple_tab(scrollable(self.tab_simple), self.v["simple_build"],
                         self.v["simple_iface"],
                         self.v["simple_lazyrad"], self.v["simple_shell"],
                         self.v["simple_devices"], self.v["simple_doom"],
                         self.v["simple_modplayer"], self.v["simple_net"], self._run,
                         self.v["simple_linuxapps"], self.v["simple_hidpi"],
                         self.v["simple_tls"], self.v["simple_lazyweb"],
                         self.v["simple_mail"], self.v["simple_traydemo"])
        self._build_left(scrollable(tab_adv))
        self._build_right(right)

    def _build_left(self, parent: ttk.Frame) -> None:
        """Mode, build switches, session script, and run-option groups."""
        g = self._group(parent, "Mode")
        self.cmb_mode = ttk.Combobox(g, textvariable=self.v["mode"], state="readonly",
                                     values=[m[0] for m in MODES])
        self.cmb_mode.pack(fill="x", padx=6, pady=(4, 2))
        self.lbl_mode = ttk.Label(g, wraplength=520, foreground="#444")
        self.lbl_mode.pack(fill="x", padx=6, pady=(0, 6))

        g = self._group(parent, "Image configuration (build switches)")
        self._check(g, "Desktop profile (LAZYOS_DESKTOP)", "desktop")
        self._check(g, "  LazyShell desktop (off = LAZYOS_SHELL=0)", "shell")
        self._check(g, "Services session (LAZYOS_SERVICES)", "services")
        self._check(g, "Compositor (LAZYOS_XUID)", "xuid")
        self._check(g, "Shell probe (+ LAZYOS_SHELLPROBE)", "shellprobe")
        self._check(g, "Messengerctl demo (LAZYOS_MESSENGERCTL)", "msgctl")
        self._check(g, "Messengerd daemon (LAZYOS_MESSENGERD)", "msgrd")
        self._check(g, "Compositor client (+ LAZYOS_XUI_CLIENT)", "xui_client")
        self._check(g, "LazyRAD IDE + player (LAZYOS_LAZYRAD)", "lazyrad")
        self._check(g, "Doom package in /system/share/samples (LAZYOS_DOOM)", "doom")
        self._check(g, "LazyRAD MOD player package in /system/share/samples (LAZYOS_MODPLAYER)",
                    "modplayer")
        self._check(g, "Mail app, esMail over TLS (desktop; LAZYOS_MAIL)", "mail")
        self._check(g, "Tray demo, the tray sample app (desktop; LAZYOS_TRAYDEMO)", "traydemo")
        self._check(g, "USB stick image too (LAZYOS_USB_IMAGE)", "usb_image")
        self._check(g, "Linux programs dash/lua/sqlite3/jq/rg (LAZYOS_LINUXAPPS)", "linuxapps")
        self._check(g, "ext2 journal on the OS volume (LAZYOS_JOURNAL)", "journal")
        self._check(g, "Devices app at boot (desktop; LAZYOS_XUI_AUTOSTART += devices)",
                    "devices")
        row = ttk.Frame(g); row.pack(fill="x", padx=6, pady=2)
        ttk.Label(row, text="LazyRAD samples:").pack(side="left")
        ttk.Entry(row, textvariable=self.v["lazyrad_samples"]).pack(side="left", fill="x",
                                                                   expand=True, padx=6)
        row = ttk.Frame(g); row.pack(fill="x", padx=6, pady=2)
        ttk.Label(row, text="XUI app:").pack(side="left")
        ttk.Combobox(row, textvariable=self.v["xui_app"], state="readonly",
                     values=XUI_VIEWERS, width=14).pack(side="left", padx=6)
        ttk.Label(g, text="Build the xui app first ('Build xui app' mode) before booting it.",
                  foreground="#666").pack(fill="x", padx=6)
        row = ttk.Frame(g); row.pack(fill="x", padx=6, pady=2)
        ttk.Label(row, text="BusyBox:").pack(side="left")
        ttk.Entry(row, textvariable=self.v["busybox"]).pack(side="left", fill="x",
                                                           expand=True, padx=6)

        netopts.build_group(self._group(parent, "Networking (QEMU user network)"),
                            *(self.v[k] for k in ("net", "net_forwards", "net_restrict", "tls",
                                                    "lazyweb")))

        driveropts.build_group(self._group(parent, "Drivers (issue #497)"),
                               *(self.v[k] for k in ("sound_card", "nic", "devd")))

        self.g_test = self._group(parent, "Test app / session script")
        self.cmb_script = ttk.Combobox(self.g_test, textvariable=self.v["script"],
                                       state="readonly", values=[s[1] for s in SCRIPTS])
        self.cmb_script.pack(fill="x", padx=6, pady=(4, 2))
        ttk.Label(self.g_test, text="Selecting a script applies the build switches it needs.",
                  foreground="#666").pack(fill="x", padx=6, pady=(0, 6))

        g = self._group(parent, "Home volume (persistent ext2 disk mounted at /home)")
        self.lbl_volume = datavol.build_group(g, self.v["home_path"], self.v["home_disk"],
                                              self._reset_volume)
        row = ttk.Frame(g); row.pack(fill="x", padx=6, pady=(0, 2))
        ttk.Checkbutton(row, text="Also attach legacy data volume:",
                        variable=self.v["data_disk"]).pack(side="left")
        ttk.Entry(row, textvariable=self.v["data_path"]).pack(side="left", fill="x",
                                                              expand=True, padx=(6, 0))
        row = ttk.Frame(g); row.pack(fill="x", padx=6, pady=(0, 6))
        ttk.Checkbutton(row, text="Recreate the OS volume (erases installed apps, settings, "
                        "logs and /data; needs a build)",
                        variable=self.v["reset_os"]).pack(side="left")

        g = self._group(parent, "Run options")
        row = ttk.Frame(g); row.pack(fill="x", padx=6, pady=2)
        ttk.Label(row, text="Profile:").pack(side="left")
        ttk.Combobox(row, textvariable=self.v["profile"], state="readonly",
                     values=["dev", "release"], width=10).pack(side="left", padx=(4, 12))
        ttk.Label(row, text="Accel:").pack(side="left")
        ttk.Combobox(row, textvariable=self.v["accel"], state="readonly",
                     values=ACCELS, width=8).pack(side="left", padx=4)
        ttk.Label(row, text="Disk:").pack(side="left", padx=(8, 0))
        ttk.Combobox(row, textvariable=self.v["disk"], state="readonly",
                     values=DISKS, width=8).pack(side="left", padx=4)
        self._field(g, "Memory:", "memory", 6)
        self._field(g, "Capture at:", "times", 8)
        self._field(g, "Timeout (s):", "timeout", 6)
        self._field(g, "QEMU path:", "qemu", 44, browse=self._browse_qemu)
        self._field(g, "Output dir:", "out", 44, browse=self._browse_out)
        self._field(g, "QEMU args:", "extra", 44)
        self._field(g, "Kernel limits:", "limits", 44)  # heap_max=512M fd_max=4096 ...
        self._field(g, "Display mode:", "display_mode", 12)  # 2560x1440: HiDPI, 720p at 2x
        self._field(g, "Asset dirs:", "assets", 44)  # dir;dir, each with manifest.txt (#454)
        row = ttk.Frame(g); row.pack(fill="x", padx=6, pady=2)
        ttk.Checkbutton(row, text="Skip build", variable=self.v["skip_build"]).pack(side="left")
        ttk.Checkbutton(row, text="Headless", variable=self.v["headless"]).pack(side="left", padx=12)
        ttk.Checkbutton(row, text="USB tablet", variable=self.v["tablet"]).pack(side="left")
        ttk.Checkbutton(row, text="Sound card", variable=self.v["sound"]).pack(side="left", padx=12)
        row = ttk.Frame(g); row.pack(fill="x", padx=6, pady=2)
        ttk.Label(row, text="ABI at (s):").pack(side="left")
        ttk.Entry(row, textvariable=self.v["abi_time"], width=5).pack(side="left", padx=4)
        ttk.Label(row, text="only:").pack(side="left", padx=(8, 2))
        ttk.Entry(row, textvariable=self.v["abi_only"], width=24).pack(side="left")
        ttk.Checkbutton(row, text="Build fixtures",
                        variable=self.v["abi_build"]).pack(side="left", padx=12)

    def _build_right(self, parent: ttk.Frame) -> None:
        """The plan preview (top) and the scrollable output log (fills)."""
        ttk.Label(parent, text="Plan preview:").pack(anchor="w", padx=4)
        self.txt_plan = tk.Text(parent, height=5, wrap="none", font=("Consolas", 8),
                                background="#f4f4f4", relief="solid", borderwidth=1)
        self.txt_plan.pack(fill="x", padx=4, pady=(0, 4))
        self.txt_plan.configure(state="disabled")
        ttk.Label(parent, text="Output:").pack(anchor="w", padx=4)
        wrap = ttk.Frame(parent)
        wrap.pack(fill="both", expand=True, padx=4, pady=(0, 4))
        self.log = tk.Text(wrap, wrap="none", font=("Consolas", 9),
                           background="#121212", foreground="#dcdcdc",
                           insertbackground="#dcdcdc")
        ysb = ttk.Scrollbar(wrap, orient="vertical", command=self.log.yview)
        xsb = ttk.Scrollbar(wrap, orient="horizontal", command=self.log.xview)
        self.log.configure(yscrollcommand=ysb.set, xscrollcommand=xsb.set)
        self.log.grid(row=0, column=0, sticky="nsew")
        ysb.grid(row=0, column=1, sticky="ns")
        xsb.grid(row=1, column=0, sticky="ew")
        wrap.rowconfigure(0, weight=1)
        wrap.columnconfigure(0, weight=1)
        for tag, color in (("out", "#dcdcdc"), ("err", "#ff8a80"), ("cmd", "#4fc3f7"),
                           ("head", "#ffd54f"), ("ok", "#81c784"), ("dim", "#888"),
                           ("fail", "#ef5350")):
            self.log.tag_configure(tag, foreground=color)

    def _build_bottom(self) -> None:
        """The Run/Build/Stop/Open/Clear button bar and the status label."""
        bar = ttk.Frame(self.root)
        bar.pack(side="bottom", fill="x")
        self.btn_run = ttk.Button(bar, text="Run", command=self._run)
        self.btn_run.pack(side="left", padx=(6, 2), pady=6)
        self.btn_build = ttk.Button(bar, text="Build image", command=self._build_image)
        self.btn_build.pack(side="left", padx=2, pady=6)
        self.btn_stop = ttk.Button(bar, text="Stop", command=self._stop, state="disabled")
        self.btn_stop.pack(side="left", padx=2, pady=6)
        ttk.Button(bar, text="Open output", command=self._open_out).pack(side="left", padx=2, pady=6)
        ttk.Button(bar, text="Clear log", command=self._clear).pack(side="left", padx=2, pady=6)
        self.status = ttk.Label(bar, text="Ready.", anchor="e", foreground="#333")
        self.status.pack(side="right", padx=10)

    # -------------------------------------------------------------- helpers
    @staticmethod
    def _group(parent: ttk.Frame, title: str) -> ttk.LabelFrame:
        """A packed, full-width labeled group box."""
        g = ttk.LabelFrame(parent, text=title)
        g.pack(fill="x", padx=4, pady=4)
        return g

    def _check(self, parent: ttk.Frame, text: str, key: str) -> None:
        """A checkbox bound to config variable ``key``."""
        ttk.Checkbutton(parent, text=text, variable=self.v[key]).pack(anchor="w", padx=6)

    def _field(self, parent: ttk.Frame, label: str, key: str, width: int, browse=None) -> None:
        """A labeled entry bound to ``key``, with an optional browse button."""
        row = ttk.Frame(parent)
        row.pack(fill="x", padx=6, pady=2)
        ttk.Label(row, text=label, width=12).pack(side="left")
        ttk.Entry(row, textvariable=self.v[key], width=width).pack(side="left", fill="x", expand=True)
        if browse:
            ttk.Button(row, text="...", width=3, command=browse).pack(side="left", padx=(4, 0))

    def _bind(self) -> None:
        """Wire selection/trace callbacks and the polling timer."""
        self.cmb_mode.bind("<<ComboboxSelected>>", lambda e: self._on_mode())
        self.cmb_script.bind("<<ComboboxSelected>>", lambda e: self._on_script())
        self.notebook.bind("<<NotebookTabChanged>>", lambda e: self._update_plan())
        self.v["home_path"].trace_add("write", lambda *_: self._refresh_volume())
        for var in self.v.values():
            var.trace_add("write", lambda *_: self._update_plan())
        self.root.protocol("WM_DELETE_WINDOW", self._on_close)
        self.root.after(100, self._poll)

    # ------------------------------------------------------------ behaviour
    def _on_mode(self) -> None:
        """Show the mode description and enable only its relevant controls."""
        mode = self.v["mode"].get()
        self.lbl_mode.configure(text=dict(MODES)[mode])
        session = mode == "Scripted session"
        self.cmb_script.configure(state="readonly" if session else "disabled")
        self.g_test.configure(relief="groove" if session else "flat")
        self._update_plan()

    def _on_script(self) -> None:
        """Apply the build switches and xui app a session script requires."""
        match = [s for s in SCRIPTS if s[1] == self.v["script"].get()]
        if not match:
            return
        _, _, switches, xui = match[0]
        desktop = "desktop" in switches
        self.v["desktop"].set(desktop)
        self.v["shell"].set(True)
        self.v["xui_autostart"].set(xui if desktop else "")
        self.v["services"].set("services" in switches)
        self.v["xuid"].set("xuid" in switches)
        self.v["shellprobe"].set("shellprobe" in switches)
        self.v["msgctl"].set(False)
        self.v["msgrd"].set(False)
        self.v["xui_client"].set("xui_client" in switches)
        self.v["traydemo"].set("LAZYOS_TRAYDEMO" in SCRIPT_ENV.get(match[0][0], {}))
        self.v["xui_app"].set("(none)" if desktop else xui or "(none)")
        self._update_plan()

    def _update_plan(self) -> None:
        """Re-render the command preview; never let a plan error break tracing."""
        try:
            text = format_plan(build_plan(self.cfg()))
        except Exception as exc:  # never let a UI traceback break tracing
            text = f"(plan error: {exc})"
        self.txt_plan.configure(state="normal")
        self.txt_plan.delete("1.0", "end")
        self.txt_plan.insert("1.0", text)
        self.txt_plan.configure(state="disabled")

    def _run(self) -> None:
        """Start the current plan, or report a malformed one without crashing."""
        if self.runner.busy:
            return
        try:
            steps = build_plan(self.cfg())
        except ValueError as exc:  # e.g. an unmatched quote in the QEMU args
            self._log(f"(plan error: {exc})\n", "fail")
            return
        if not datavol.confirm_reset_os(self.cfg()):
            self._log("OS volume reset cancelled.\n", "fail")
            return
        if not steps:
            self._log("Nothing to run.\n", "fail")
            return
        self._begin(len(steps), f"LazyOS run: {self.cfg()['mode']}")
        self.runner.start(steps, build_env(self.cfg()), ROOT)

    def _build_image(self) -> None:
        """Build just target/lazyos.img with the current switches."""
        try:  # bad kernel limits: report before the buttons go busy
            steps, env = image_build(self.cfg())
        except ValueError as exc:
            self._log(f"(plan error: {exc})\n", "fail")
            return
        if not self.runner.busy:
            self._begin(len(steps), "Build image")
            self.runner.start(steps, env, ROOT)

    def _begin(self, count: int, title: str) -> None:
        """Log a run banner and switch the buttons into the busy state."""
        self._log(f"\n######## {title} ({count} step(s)) ########\n", "head")
        self.btn_run.configure(state="disabled")
        self.btn_build.configure(state="disabled")
        self.btn_stop.configure(state="normal")

    def _reset_volume(self) -> None:
        """Format an empty home volume after confirmation, then refresh the label."""
        outcome = datavol.reset(self.v["home_path"].get().strip(), self.runner.busy)
        if outcome:
            succeeded, message = outcome
            self._log(message + "\n", "ok" if succeeded else "fail")
        self._refresh_volume()

    def _refresh_volume(self) -> None:
        """Show the volume's path, size and existence under its entry."""
        self.lbl_volume.configure(text=datavol.status_text(self.v["home_path"].get().strip()))

    def _stop(self) -> None:
        """Cancel the run and kill its process tree."""
        self.runner.stop()
        self._log("\n(stopped by user)\n", "err")

    def _open_out(self) -> None:
        """Reveal the output directory in the platform file manager."""
        path = self.v["out"].get().strip() or "shots"
        if not os.path.isabs(path):
            path = os.path.join(ROOT, path)
        try:
            open_path(path)
        except Exception as exc:
            self._log(f"cannot open {path}: {exc}\n", "err")

    def _clear(self) -> None:
        """Empty the log and reset the status line."""
        self.log.configure(state="normal")
        self.log.delete("1.0", "end")
        self.log.configure(state="disabled")
        self.status.configure(text="Ready.")

    def _browse_qemu(self) -> None:
        """Pick the qemu-system-x86_64 executable."""
        path = filedialog.askopenfilename(title="Select qemu-system-x86_64")
        if path:
            self.v["qemu"].set(path)

    def _browse_out(self) -> None:
        """Pick the output directory."""
        path = filedialog.askdirectory(title="Select output directory")
        if path:
            self.v["out"].set(path)

    def _log(self, text: str, tag: str = "out") -> None:
        """Append ``text`` to the read-only log under the given color tag."""
        if not text:
            return
        self.log.configure(state="normal")
        self.log.insert("end", text, (tag,))
        self.log.see("end")
        self.log.configure(state="disabled")

    def _poll(self) -> None:
        """Drain a bounded batch of runner messages, then reschedule.

        The batch cap keeps a step that out-produces this loop from starving the
        Tk event loop, so Stop and window-close still get processed.
        """
        for _ in range(1000):
            try:
                msg = self.runner.q.get_nowait()
            except queue.Empty:
                break
            kind = msg[0]
            if kind == "step":
                self._log(f"\n=== {msg[1]} ===\n", "cmd")
                self._log(msg[2] + "\n", "dim")
                self.status.configure(text=f"Running: {msg[1]}")
            elif kind == "out":
                self._log(msg[1], "out")
            elif kind == "exit":
                tag = "dim" if msg[1] == 0 else "fail"
                self._log(f"(exit code {msg[1]})\n", tag)
            elif kind == "done":
                self._finish(msg[1])
        self.root.after(100, self._poll)

    def _finish(self, code: int) -> None:
        """Restore the buttons after a run and report the final status."""
        self.btn_run.configure(state="normal")
        self.btn_build.configure(state="normal")
        self.btn_stop.configure(state="disabled")
        if code == 0:
            self._log("\nDone.\n", "ok")
            self.status.configure(text="Done.")
        else:
            self._log(f"\nFinished with errors (exit {code}).\n", "fail")
            self.status.configure(text=f"Failed (exit {code}).")

    def _on_close(self) -> None:
        """Kill any running child, then tear the window down."""
        self.runner.stop()
        self.root.destroy()


def main() -> int:
    """Open the launcher window and block until it is closed."""
    root = tk.Tk()
    Launcher(root)
    root.mainloop()
    return 0
