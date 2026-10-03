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

sys.path.insert(0, os.path.join(ROOT, "tools", "net"))
import qemu_net  # noqa: E402  (the QEMU network arguments run_demo and the tools share)

#: LazyOS-only LazyRAD sample projects (`lazyrad-os/samples/`), embedded under
#: `/system/share/lazyrad/` with every LazyRAD image next to any the user lists. Relative
#: entries resolve against the repo root (`build_support/lazyrad_embed.rs`).
LAZYOS_LAZYRAD_SAMPLES = ("lazyrad-os/samples/messenger", "lazyrad-os/samples/modplayer")
CARGO = shutil.which("cargo") or "cargo"
IMAGE = os.path.join(ROOT, "target", "lazyos.img")
# The persistent ext2 home volume (mounted at /home); `run_demo.py` creates it
# on first use. The legacy data volume is opt-in and mounted nowhere new.
HOME_IMAGE = os.path.join(ROOT, "target", "home.img")
DATA_IMAGE = os.path.join(ROOT, "target", "data.img")

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
# script needs; `xui-app` names the viewer to embed via LAZYOS_XUI_APP. A
# `desktop` script (the document apps) instead boots the desktop profile with
# every desktop app embedded and only `xui-app` autostarted.
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
    ("xui_sysmon.json", "XUI app: sysmon (tasks, memory, services)", ("xuid", "services"), "sysmon"),
    ("xui_fabricmon.json", "XUI app: fabricmon (services)", ("xuid", "services"), "fabricmon"),
    ("xui_client.json", "XUI app: compositor client (window/focus)", ("xuid", "xui_client"), "client"),
    ("xui_editor.json", "XUI app: Editor (type, save)", ("desktop",), "editor"),
    ("xui_paint.json", "XUI app: Paint (draw, save PNG)", ("desktop",), "paint"),
    ("xui_files.json", "XUI app: Files (browse, open)", ("desktop",), "files"),
    ("xui_writer.json", "XUI app: LazyWriter (format, save, export)", ("desktop",), "writer"),
    ("xui_settings.json", "XUI app: Settings (menu, colours, layout)", ("desktop",), None),
    ("shell_demo.json", "LazyShell (taskbar, start menu, restart)", ("desktop",), "term"),
    ("xui_settings_time.json", "XUI app: Settings (time, clock format, light mode)",
     ("desktop",), None),
    ("xui_settings_hidden.json", "XUI app: Settings (hide an app from the start menu)",
     ("desktop",), None),
]

# Simple mode: (label, cargo profile) and (label, description) choices.
SIMPLE_BUILDS = [("Debug", "dev"), ("Release", "release")]
SIMPLE_INTERFACES = [
    ("CLI",
     "A basic terminal screen with the system shell (busybox sh) connected to it."),
    ("Desktop",
     "The full services suite (init, messengerd, logd, healthd, keyd, accounts, "
     "clipboardd, ...) plus the xuid compositor, the LazyShell desktop and an XUI "
     "app window."),
]

XUI_VIEWERS = ["(none)", "m0", "counter", "sysmon", "fabricmon", "client", "term",
               "editor", "paint", "files", "writer", "settings", "devices"]
# What the desktop opens at boot when the Devices app is asked for (issue
# #481): the Terminal first (it takes the focus), then Devices. Matches
# `run_demo.py --devices`.
DEVICES_AUTOSTART = "term,devices"
# The desktop session's apps (issues #215/#216): embedded side by side, opened
# by `init` as `xuid` clients. The Terminal comes first so it takes the focus.
# The document apps ship with every desktop image (`build.rs`
# `SHIP_DOCUMENT_APPS`); they open on demand (Start menu, right-click menu,
# open-with), never at boot. The GUI does not list the embedded apps: the
# desktop profile (`LAZYOS_DESKTOP=1`) makes `build.rs` embed its own default
# set, so a new app needs no change here.
DOCUMENT_APPS = ("editor", "files", "paint", "writer")
ACCELS = ["auto", "none", "tcg", "whpx", "kvm"]
DISKS = ["virtio", "ata"]


def build_env(cfg: dict) -> dict[str, str]:
    """The LAZYOS_* environment for an image build / run."""
    env: dict[str, str] = {}
    if cfg.get("desktop"):
        # One switch expands to the desktop recipe (issue #217): the image
        # build and `init` derive the rest from it.
        env["LAZYOS_DESKTOP"] = "1"
        # LazyShell (issue #157) is part of the profile; unchecking it opts out
        # (the compositor then shows background and windows only).
        if not cfg.get("shell", True):
            env["LAZYOS_SHELL"] = "0"
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
    if cfg.get("xui_autostart"):
        # A document-app session: the desktop profile opens just this app, but
        # the full app set is embedded (Files' open-with needs the Editor).
        env["LAZYOS_XUI_AUTOSTART"] = cfg["xui_autostart"]
    if cfg.get("devices") and cfg.get("desktop"):
        # The Devices app ships with every desktop image; this opens it at
        # boot, next to whatever else the session opens.
        current = env.get("LAZYOS_XUI_AUTOSTART", "term")
        if "devices" not in current.split(","):
            env["LAZYOS_XUI_AUTOSTART"] = f"{current},devices"
    if cfg["busybox"]:
        env["LAZYOS_BUSYBOX"] = cfg["busybox"]
    if cfg.get("cli"):
        env["LAZYOS_CLI"] = "1"
    if cfg.get("lazyrad"):
        # Ships the core package os.lazy.lazyrad (the IDE and its player, built by
        # `tools/lazyrad/build.py`, which repackages the core packages): `pkgd`
        # installs it at boot, so Settings -> Menu offers it. Needs the desktop
        # profile's core packages (`tools/xui/build.py`).
        env["LAZYOS_LAZYRAD"] = "1"
        env["LAZYRAD_SAMPLES"] = lazyrad_samples(cfg.get("lazyrad_samples", ""))
    if cfg.get("doom"):
        # Places the Doom package (built by `tools/doom/build.py`) in
        # /system/share/samples; a user installs it through pkgd.
        env["LAZYOS_DOOM"] = "1"
    if cfg.get("modplayer"):
        # Places modplayer.lzp (built by `tools/lazyrad/package.py`) in /system/share/samples on the OS
        # volume; the player it carries is LazyRAD's, so LazyRAD comes too.
        env["LAZYOS_MODPLAYER"] = "1"
        env["LAZYOS_LAZYRAD"] = "1"
        env["LAZYRAD_SAMPLES"] = lazyrad_samples(cfg.get("lazyrad_samples", ""))
    if cfg.get("net"):
        # The network stack (driver, `netd`, the shell tools and, on the
        # desktop, the Network and Net Tools apps). `demo=0` leaves out the
        # evidence clients that need `tools/net/run.py`'s host servers.
        env["LAZYOS_NETD"] = "1"
        env["LAZYOS_NETD_ARGS"] = "demo=0"
    return env


def net_specs(cfg: dict) -> list[str]:
    """The port forwards typed in the launcher (space- or comma-separated);
    empty means run_demo's default (host 8080 to the Net Tools server)."""
    return [spec for spec in cfg.get("net_forwards", "").replace(",", " ").split() if spec]


def net_flags(cfg: dict) -> list[str]:
    """The `--net` flags run_demo and the screenshot tools share, or none.
    A malformed forward raises ValueError (shown as a plan error)."""
    if not cfg.get("net"):
        return []
    specs = net_specs(cfg)
    qemu_net.forwards_from(specs)  # validate now, not after a long build
    flags = ["--net"]
    for spec in specs:
        flags += ["--net-forward", spec]
    if cfg.get("net_restrict"):
        flags.append("--net-restrict")
    return flags


def lazyrad_samples(user: str) -> str:
    """`LAZYRAD_SAMPLES`: the user's list, then the LazyOS samples it lacks."""
    entries = [entry for entry in user.split(os.pathsep) if entry]
    entries += [entry for entry in LAZYOS_LAZYRAD_SAMPLES if entry not in entries]
    return os.pathsep.join(entries)


def simple_config(base: dict, build: str, interface: str, lazyrad: bool = False,
                  shell: bool = True, devices: bool = False, doom: bool = False,
                  modplayer: bool = False, net: bool = False) -> dict:
    """The full configuration for a Simple-mode choice.

    ``build`` is a cargo profile (``dev``/``release``) and ``interface`` is
    ``CLI`` or ``Desktop``; ``lazyrad`` adds the LazyRAD IDE to a Desktop
    image (a core package like the other desktop apps, so it means nothing on
    the CLI), ``shell``
    keeps the LazyShell desktop (taskbar, start menu) on it, ``devices``
    opens the Devices app at boot, ``doom`` adds the Doom package and
    ``modplayer`` the LazyRAD MOD player package (likewise Desktop only); ``net``
    adds networking to either interface (the stack, QEMU's user network with
    host port 8080 forwarded, and on the desktop the Network and Net Tools
    apps). Machine settings (accelerator, memory, QEMU path)
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
        # The desktop gets a sound card (type `beep` in the Terminal); the CLI
        # image stays quiet, since a sound card there means boot-time test tones.
        "sound": desktop,
        # Desktop = the single `LAZYOS_DESKTOP=1` profile (issue #217): services
        # suite + compositor + the xui apps as its clients (`init` opens the
        # Terminal at boot; the viewers, Editor, Files and Paint are embedded
        # and open on demand), with no demo/evidence programs. The individual switches stay off so no
        # Advanced checkbox leaks in.
        "desktop": desktop,
        "services": False,
        "xuid": False,
        "shellprobe": False,
        "msgctl": False,
        "msgrd": False,
        "xui_client": False,
        "xui_app": "(none)",
        "prebuild_xui": desktop,
        "lazyrad": desktop and lazyrad,
        "shell": desktop and shell,
        "devices": desktop and devices,
        "doom": desktop and doom,
        "modplayer": desktop and modplayer,
        "net": net,
        "net_forwards": "",
        "net_restrict": False,
    })
    return cfg


def cargo_step(cfg: dict) -> dict:
    """The `cargo build` step that produces target/lazyos.img."""
    argv = [CARGO, "build"]
    if cfg["profile"] == "release":
        argv.append("--release")
    return {"label": "Build image (cargo build)", "argv": argv}


def xui_steps() -> list[dict]:
    """Build the xui apps, then package the desktop apps as core packages
    (issue #509): `target/pkg/core/*.lzp`, which a desktop image embeds in
    `/system/packages` and `pkgd` installs at boot."""
    return [{"label": "Build xui apps (static musl)",
             "argv": [PY, "tools/xui/build.py", "--no-core-packages"]},
            core_packages_step()]


def core_packages_step() -> dict:
    """Package the built desktop apps (`tools/xui/core_packages.py`): the
    desktop image build needs `target/pkg/core`."""
    return {"label": "Build core packages", "argv": [PY, "tools/xui/core_packages.py"]}


def lazyrad_step(cfg: dict) -> list[dict]:
    """The step that builds LazyRAD's static-musl ELFs, when the image embeds them."""
    if not cfg.get("lazyrad") and not cfg.get("modplayer"):
        return []
    return [{"label": "Build LazyRAD (static musl)",
             "argv": [PY, "tools/lazyrad/build.py"]}]


def doom_step(cfg: dict) -> list[dict]:
    """The step that builds the Doom package, when the image embeds it."""
    if not cfg.get("doom"):
        return []
    return [{"label": "Build Doom package (engine + Freedoom)",
             "argv": [PY, "tools/doom/build.py", "--require"]}]


def modplayer_step(cfg: dict) -> list[dict]:
    """The step that packages the LazyRAD MOD player, when the image embeds it
    (after `lazyrad_step`: the package carries the player it built)."""
    if not cfg.get("modplayer"):
        return []
    return [{"label": "Package the MOD player (LazyRAD)",
             "argv": [PY, "tools/lazyrad/package.py", "--no-build", "--require"]}]


def app_steps(cfg: dict) -> list[dict]:
    """Every optional app the image embeds, built before `cargo build`."""
    return lazyrad_step(cfg) + modplayer_step(cfg) + doom_step(cfg)


def _script(cfg: dict) -> tuple:
    """The SCRIPTS entry selected by ``cfg["script"]``."""
    return SCRIPTS[cfg["script"]]


def build_plan(cfg: dict) -> list[dict]:
    """Turn a configuration dict into an ordered list of steps."""
    mode = cfg["mode"]
    steps: list[dict] = []

    if mode == "Interactive demo":
        if cfg.get("prebuild_xui"):
            steps += xui_steps()
        argv = [PY, "tools/run_demo.py"]
        if cfg.get("lazyrad") and not cfg["skip_build"]:
            # run_demo builds LazyRAD and sets LAZYOS_LAZYRAD itself; with
            # "Skip build" the existing image is booted as it is.
            argv.append("--lazyrad")
        if cfg.get("doom") and not cfg["skip_build"]:
            # run_demo builds the package and sets LAZYOS_DOOM itself.
            argv.append("--doom")
        if cfg.get("modplayer") and not cfg["skip_build"]:
            # run_demo builds LazyRAD and the package and sets the switches.
            argv.append("--modplayer")
        if cfg.get("devices") and cfg.get("desktop") and not cfg["skip_build"]:
            # run_demo builds the xui apps and opens Devices at boot itself.
            argv.append("--devices")
        if cfg["profile"] == "release":
            argv.append("--release")
        if cfg["skip_build"]:
            argv.append("--no-build")
        elif cfg.get("desktop") and not cfg.get("shell", True):
            # Also set by `build_env`; the flag keeps the command line honest.
            argv.append("--no-shell")
        if cfg["headless"]:
            argv.append("--headless")
        argv += ["--accel", cfg["accel"], "--memory", cfg["memory"],
                 "--disk", cfg.get("disk", "virtio")]
        # Only the interactive demo persists state; the scripted modes stay
        # hermetic unless a script asks for a volume itself.
        if cfg.get("home_disk", True):
            argv += ["--home-disk", cfg.get("home_path") or HOME_IMAGE]
        else:
            argv.append("--no-home-disk")
        if cfg.get("data_disk", False):
            argv += ["--data-disk", cfg.get("data_path") or DATA_IMAGE]
        # Recreate the OS volume (apps, settings, logs, /data) instead of the
        # in-place update; it needs a build, so "Skip build" wins.
        # run_demo asks before erasing and has no terminal here, so the GUI asks
        # first (datavol.confirm_reset_os) and passes --yes on its behalf.
        if cfg.get("reset_os") and not cfg["skip_build"]:
            argv += ["--reset-os", "--yes"]
        # A virtio-sound card on the host's audio backend. run_demo also builds
        # with LAZYOS_SOUND=1; the desktop profile ships the sound stack anyway,
        # and on other images the driver plays its boot tones.
        if cfg.get("sound"):
            argv.append("--sound")
        # A virtio-net card on QEMU's user network with the forwards; run_demo
        # also builds the network stack (and the network apps on a desktop).
        argv += net_flags(cfg)
        if cfg["qemu"]:
            argv += ["--qemu", cfg["qemu"]]
        if cfg["extra"]:
            argv += ["--"] + shlex.split(cfg["extra"])
        steps.append({"label": "Interactive demo", "argv": argv})

    elif mode == "Headless screenshots":
        if not cfg["skip_build"]:
            steps += app_steps(cfg)
            steps.append(cargo_step(cfg))
        argv = [PY, "tools/screenshot/qemu_shot.py", "--out", cfg["out"],
                "--at", cfg["times"], "--accel", cfg["accel"],
                "--memory", cfg["memory"], "--image", IMAGE]
        argv += net_flags(cfg)
        if cfg["qemu"]:
            argv += ["--qemu", cfg["qemu"]]
        steps.append({"label": "Capture screenshots", "argv": argv})

    elif mode == "Scripted session":
        file, label, switches, xui = _script(cfg)
        if xui:
            steps += xui_steps()
        elif "desktop" in switches:
            # The desktop apps are packages: package the apps already built.
            steps.append(core_packages_step())
        if not cfg["skip_build"]:
            steps += app_steps(cfg)
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
        argv += net_flags(cfg)
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
        steps += xui_steps()
        steps += app_steps(cfg)

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
