"""Modes, session scripts, image build switches, and command planning.

Everything here is pure (no Tkinter, no processes) so it can be unit-tested and
reasoned about on its own. The GUI builds a configuration dict, and
:func:`build_plan` turns it into an ordered list of ``{"label", "argv"}`` steps
that :class:`~lazygui.runner.Runner` executes.
"""

from __future__ import annotations

import os
import shlex
import shutil
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
PY = sys.executable
CARGO = shutil.which("cargo") or "cargo"
IMAGE = os.path.join(ROOT, "target", "lazyos.img")

MODES = [
    ("Interactive demo",
     "Build target/lazyos.img and boot it in a QEMU window. Tab switches focus; "
     "keystrokes go to the focused program."),
    ("Headless screenshots",
     "Build (unless skipped), boot headless and capture PNGs at the chosen times "
     "into the output folder."),
    ("Scripted session",
     "Build the image for the selected test app/session, then drive the guest "
     "with a scripted input timeline and capture PNGs."),
    ("Kernel test suite",
     "Build with LAZYOS_TESTS=1, run the in-kernel unit + stress/soak suite, and "
     "write docs/test/report.md."),
    ("Linux ABI bench",
     "Run the Linux-ABI conformance bench over the static musl fixtures; writes "
     "docs/compat/matrix.md."),
    ("Build xui app",
     "Build the static-musl XUI toolkit apps (m0/counter/sysmon/fabricmon) into "
     "target/xui."),
]

# (file, label, switches, xui-app) - `switches` are the image build switches a
# script needs; `xui-app` names the viewer to embed via LAZYOS_XUI_APP.
SCRIPTS = [
    ("type_and_shot.json", "Type & shot (input smoke test)", (), None),
    ("multitask_demo.json", "Multitask (two windows, Tab focus)", (), None),
    ("fs_demo.json", "Filesystem (ls / cat)", (), None),
    ("window_demo.json", "Window move / scroll", (), None),
    ("mouse_demo.json", "Mouse move", (), None),
    ("services_demo.json", "Services (health / log / registry)", ("services",), None),
    ("login_demo.json", "Login & accounts", ("services",), None),
    ("apps_demo.json", "App launch & open-with", ("services",), None),
    ("dnd_drop.json", "Drag & drop (drop)", ("xuid",), None),
    ("dnd_cancel.json", "Drag & drop (cancel)", ("xuid",), None),
    ("xuid_wm.json", "XUID window manager", ("xuid",), None),
    ("xuid_shell.json", "XUID shell protocol (Alt+F4/Tab)", ("xuid", "shellprobe"), None),
    ("xui_counter.json", "XUI app: counter (click to increment)", ("xuid",), "counter"),
    ("xui_sysmon.json", "XUI app: sysmon dashboard", ("xuid",), "sysmon"),
    ("xui_fabricmon.json", "XUI app: fabricmon (services)", ("xuid", "services"), "fabricmon"),
    ("xui_client.json", "XUI app: compositor client (window/focus)", ("xuid", "xui_client"), "client"),
]

# Simple mode: (label, cargo profile) and (label, description) choices.
SIMPLE_BUILDS = [("Debug", "dev"), ("Release", "release")]
SIMPLE_INTERFACES = [
    ("CLI",
     "A basic terminal screen with the system shell (busybox sh) connected to it."),
    ("Desktop",
     "The full services suite (init, messengerd, logd, healthd, keyd, accounts, "
     "clipboardd, ...) plus the xuid compositor and an XUI app window."),
]

XUI_VIEWERS = ["(none)", "m0", "counter", "sysmon", "fabricmon", "client", "term"]
# The desktop session's apps (issues #215/#216): embedded side by side, opened
# by `init` as `xuid` clients. The Terminal comes first so it takes the focus.
DESKTOP_APPS = ("term", "sysmon", "fabricmon", "counter")
ACCELS = ["auto", "none", "tcg", "whpx", "kvm"]
DISKS = ["virtio", "ata"]


def build_env(cfg: dict) -> dict[str, str]:
    """The LAZYOS_* environment for an image build / run."""
    env: dict[str, str] = {}
    if cfg.get("desktop"):
        # One switch expands to the desktop recipe (issue #217): the image
        # build and `init` derive the rest from it.
        env["LAZYOS_DESKTOP"] = "1"
    else:
        if cfg["services"]:
            env["LAZYOS_SERVICES"] = "1"
        if cfg["xuid"]:
            env["LAZYOS_XUID"] = "1"
        if cfg["xui_client"]:
            env["LAZYOS_XUI_CLIENT"] = "1"
        if cfg["xui_app"] != "(none)":
            env["LAZYOS_XUI_APP"] = os.path.join(ROOT, "target", "xui", f"xui-{cfg['xui_app']}.elf")
    if cfg["shellprobe"] and cfg["xuid"]:
        env["LAZYOS_SHELLPROBE"] = "1"
    if cfg["msgctl"]:
        env["LAZYOS_MESSENGERCTL"] = "1"
    if cfg["msgrd"]:
        env["LAZYOS_MESSENGERD"] = "1"
    if cfg.get("xui_apps"):
        env["LAZYOS_XUI_APPS"] = os.pathsep.join(
            os.path.join(ROOT, "target", "xui", f"xui-{app}.elf") for app in cfg["xui_apps"]
        )
    if cfg["busybox"]:
        env["LAZYOS_BUSYBOX"] = cfg["busybox"]
    if cfg.get("cli"):
        env["LAZYOS_CLI"] = "1"
    return env


def simple_config(base: dict, build: str, interface: str) -> dict:
    """The full configuration for a Simple-mode choice.

    ``build`` is a cargo profile (``dev``/``release``) and ``interface`` is
    ``CLI`` or ``Desktop``. Machine settings (accelerator, memory, QEMU path)
    come from ``base``; every image switch is decided here so stale Advanced
    checkboxes cannot leak into a Simple boot.
    """
    if build not in dict(SIMPLE_BUILDS).values():
        raise ValueError(f"unknown build profile: {build!r}")
    if interface not in dict(SIMPLE_INTERFACES):
        raise ValueError(f"unknown interface: {interface!r}")
    desktop = interface == "Desktop"
    cfg = dict(base)
    cfg.update({
        "mode": "Interactive demo",
        "profile": build,
        "skip_build": False,
        "headless": False,
        "extra": base.get("extra", ""),
        "busybox": "",
        "cli": not desktop,
        # Desktop = the single `LAZYOS_DESKTOP=1` profile (issue #217): services
        # suite + compositor + the xui apps as its clients (`init` opens the
        # default set: Terminal, System Monitor, Fabric Monitor, Counter), with
        # no demo/evidence programs. The individual switches stay off so no
        # Advanced checkbox leaks in.
        "desktop": desktop,
        "services": False,
        "xuid": False,
        "shellprobe": False,
        "msgctl": False,
        "msgrd": False,
        "xui_client": False,
        "xui_app": "(none)",
        "xui_apps": DESKTOP_APPS if desktop else (),
        "prebuild_xui": desktop,
    })
    return cfg


def cargo_step(cfg: dict) -> dict:
    """The `cargo build` step that produces target/lazyos.img."""
    argv = [CARGO, "build"]
    if cfg["profile"] == "release":
        argv.append("--release")
    return {"label": "Build image (cargo build)", "argv": argv}


def _script(cfg: dict) -> tuple:
    """The SCRIPTS entry selected by ``cfg["script"]``."""
    return SCRIPTS[cfg["script"]]


def build_plan(cfg: dict) -> list[dict]:
    """Turn a configuration dict into an ordered list of steps."""
    mode = cfg["mode"]
    steps: list[dict] = []

    if mode == "Interactive demo":
        if cfg.get("prebuild_xui"):
            steps.append({"label": "Build xui apps (static musl)",
                          "argv": [PY, "tools/xui/build.py"]})
        argv = [PY, "tools/run_demo.py"]
        if cfg["profile"] == "release":
            argv.append("--release")
        if cfg["skip_build"]:
            argv.append("--no-build")
        if cfg["headless"]:
            argv.append("--headless")
        argv += ["--accel", cfg["accel"], "--memory", cfg["memory"],
                 "--disk", cfg.get("disk", "virtio")]
        if cfg["qemu"]:
            argv += ["--qemu", cfg["qemu"]]
        if cfg["extra"]:
            argv += ["--"] + shlex.split(cfg["extra"])
        steps.append({"label": "Interactive demo", "argv": argv})

    elif mode == "Headless screenshots":
        if not cfg["skip_build"]:
            steps.append(cargo_step(cfg))
        argv = [PY, "tools/screenshot/qemu_shot.py", "--out", cfg["out"],
                "--at", cfg["times"], "--accel", cfg["accel"],
                "--memory", cfg["memory"], "--image", IMAGE]
        if cfg["qemu"]:
            argv += ["--qemu", cfg["qemu"]]
        steps.append({"label": "Capture screenshots", "argv": argv})

    elif mode == "Scripted session":
        file, label, _, xui = _script(cfg)
        if xui:
            steps.append({"label": "Build xui app (static musl)",
                          "argv": [PY, "tools/xui/build.py"]})
        if not cfg["skip_build"]:
            steps.append(cargo_step(cfg))
        script = os.path.join(ROOT, "tools", "screenshot", "examples", file)
        argv = [PY, "tools/screenshot/qemu_session.py", "--image", IMAGE,
                "--out", cfg["out"], "--accel", cfg["accel"],
                "--memory", cfg["memory"], "--timeout", cfg["timeout"],
                "--script", script]
        if cfg["qemu"]:
            argv += ["--qemu", cfg["qemu"]]
        if cfg["tablet"]:
            argv.append("--tablet")
        steps.append({"label": f"Session: {label}", "argv": argv})

    elif mode == "Kernel test suite":
        argv = [PY, "tools/test/run.py", "--accel", cfg["accel"], "--image", IMAGE]
        if cfg["skip_build"]:
            argv.append("--no-build")
        steps.append({"label": "Kernel test suite", "argv": argv})

    elif mode == "Linux ABI bench":
        if cfg["abi_build"]:
            steps.append({"label": "Build ABI fixtures", "argv": [PY, "tools/abi/build.py"]})
        argv = [PY, "tools/abi/run.py"]
        if cfg["abi_time"]:
            argv += ["--at", cfg["abi_time"]]
        if cfg["abi_only"]:
            argv += ["--only", cfg["abi_only"]]
        steps.append({"label": "Linux ABI bench", "argv": argv})

    elif mode == "Build xui app":
        steps.append({"label": "Build xui app (static musl)", "argv": [PY, "tools/xui/build.py"]})

    return steps


def format_plan(steps: list[dict]) -> str:
    """Render a plan as numbered, shell-quoted lines for the preview pane."""
    if not steps:
        return "(nothing to run)"
    lines = []
    for i, step in enumerate(steps, 1):
        lines.append(f"[{i}] {step['label']}")
        lines.append("    " + shlex.join(step["argv"]))
    return "\n".join(lines)
