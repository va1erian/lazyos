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

from .assets import assets_argv, assets_env, needs_build  # noqa: F401 (re-exported)
from .display import HIDPI_MODE, check_mode, display_env  # noqa: F401 (re-exported)
from .limits import LIMIT_KEYS, limit_env  # noqa: F401 (re-exported)
from .login import DEFAULT_ACCOUNT, login_argv, login_env  # noqa: F401 (re-exported)
from .simplecfg import SIMPLE_BUILDS, SIMPLE_INTERFACES, simple_config  # noqa: F401 (re-exported)
from .appsteps import app_steps, desktop_app_argv, desktop_app_env, doom_step, lazyrad_step, lazyweb_step, linuxapps_step, mail_argv, mail_env, modplayer_step, wants_traydemo, tls_step  # noqa: F401,E501
from .scriptenv import script_env
from .netplan import net_flags, net_specs, qemu_net, wants_net, wants_tls  # noqa: F401 (re-exported)
from .drivers import device_flags, driver_env  # noqa: F401 (re-exported)

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
PY = sys.executable

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
# every desktop app embedded and only `xui-app` autostarted (`term` for the
# scripts that only wait for the Terminal: nothing opens at boot by default).
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
    ("xui_archiver.json", "XUI app: Archiver (open, extract, create, drag to Files)",
     ("desktop",), "archiver"),
    ("xui_settings.json", "XUI app: Settings (menu, colours, layout)", ("desktop",), "term"),
    ("shell_demo.json", "LazyShell (taskbar, start menu, restart)", ("desktop",), "term"),
    ("xui_settings_time.json", "XUI app: Settings (time, clock format, light mode)",
     ("desktop",), "term"),
    ("xui_settings_hidden.json", "XUI app: Settings (hide an app from the start menu)",
     ("desktop",), "term"),
    ("xui_calc.json", "XUI app: Calculator", ("desktop",), "calc"),
    ("xui_pdf.json", "XUI app: PDF Viewer", ("desktop",), "pdf"),
    ("tray.json", "Tray icons (Tray Demo)", ("desktop",), "term"),
]

XUI_VIEWERS = ["(none)", "m0", "counter", "sysmon", "fabricmon", "client", "term",
               "editor", "paint", "files", "writer", "archiver", "settings", "devices", "calc", "pdf", "traydemo"]
# What the desktop opens at boot when the Devices app is asked for (issue
# #481) and nothing else is: just Devices, since the desktop opens no app at
# boot by default. Matches `run_demo.py --devices`.
DEVICES_AUTOSTART = "devices"
# The desktop session's apps (issues #215/#216): embedded side by side, opened by `init`
# as `xuid` clients when `LAZYOS_XUI_AUTOSTART` lists them. The document apps ship with
# every desktop image (`build.rs`) and open on demand, never at boot. The GUI does not
# list the embedded apps: `LAZYOS_DESKTOP=1` makes `build.rs` embed its own default set.
DOCUMENT_APPS = ("editor", "files", "paint", "writer", "archiver")
ACCELS = ["auto", "none", "tcg", "whpx", "kvm"]
DISKS = ["virtio", "ata"]
#: Guest RAM the GUI starts with; the same as every CLI launcher's default
#: (`tools/screenshot/qemu_qmp.py` `DEFAULT_MEMORY`).
DEFAULT_MEMORY = "1G"
#: The modes whose "Skip build" boots the image already built.
SKIP_BUILD_MODES = ("Interactive demo", "Headless screenshots", "Scripted session",
                    "Kernel test suite")


def check_limits(cfg: dict) -> None:
    """Refuse kernel limits and a display mode that cannot take effect:
    malformed entries, or any entry with "Skip build", since the build writes
    them into `lazyos.cfg`."""
    skipped = cfg.get("skip_build") and cfg["mode"] in SKIP_BUILD_MODES
    if limit_env(cfg.get("limits", "")) and skipped:
        raise ValueError("kernel limits need a build: they are written into lazyos.cfg")
    if check_mode(cfg.get("display_mode", "")) and skipped:
        raise ValueError("a display mode needs a build: it is written into lazyos.cfg")
    needs_build(cfg.get("assets", ""), skipped)


def image_build(cfg: dict) -> tuple[list[dict], dict[str, str]]:
    """The "Build image" button's steps and environment (``ValueError`` on bad limits)."""
    return app_steps(cfg) + [cargo_step(cfg)], build_env(cfg)


def build_env(cfg: dict) -> dict[str, str]:
    """The LAZYOS_* environment for an image build / run."""
    env: dict[str, str] = {}
    # LazyWeb is a desktop app (docs/lazyweb.md): it brings the profile.
    if cfg.get("desktop") or cfg.get("lazyweb"):
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
        current = env.get("LAZYOS_XUI_AUTOSTART")
        if not current:
            env["LAZYOS_XUI_AUTOSTART"] = DEVICES_AUTOSTART
        elif "devices" not in current.split(","):
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
    if cfg.get("usb_image"):
        # Also writes target/lazyos-usb.img, the real-PC USB stick image
        # (docs/usb-stick.md); the run itself still boots target/lazyos.img.
        env["LAZYOS_USB_IMAGE"] = "1"
        # The stick ships `usbd` and boots `init` to start it: the target PC
        # may have no PS/2 port (the build refuses otherwise).
        env["LAZYOS_USB"] = "1"
        env.setdefault("LAZYOS_SERVICES", "1")
    if wants_net(cfg):
        # The network stack (driver, `netd`, the shell tools and, on the
        # desktop, the Network and Net Tools apps). `demo=0` leaves out the
        # evidence clients that need `tools/net/run.py`'s host servers.
        env["LAZYOS_NETD"] = "1"
        env["LAZYOS_NETD_ARGS"] = "demo=0"
    # Kernel limits for `lazyos.cfg` (Advanced tab, `run_demo.py --limit`).
    env.update(limit_env(cfg.get("limits", "")))
    # The kernel screen mode (Simple: HiDPI; Advanced: any) and the Advanced asset trees.
    env.update(display_env(cfg.get("display_mode", "")) | assets_env(cfg.get("assets", "")))
    if cfg.get("linuxapps"):
        # dash, lua, sqlite3, jq and rg (built by `tools/linuxapps/build.py`)
        # in /system/bin, on the CLI and the desktop alike.
        env["LAZYOS_LINUXAPPS"] = "1"
    if wants_tls(cfg):
        # `fetch`, `curl` and `wget` (built by `tools/nettls/build.py`) in
        # /system/bin; HTTPS needs the network stack above.
        env["LAZYOS_TLS"] = "1"
    if cfg.get("journal"):
        # An ext2 journal on the OS volume (added in place to an old image).
        env["LAZYOS_JOURNAL"] = "1"
    if cfg.get("lazyweb"):
        # The LazyWeb browser's core package (`tools/xui/build.py` builds it
        # with zig); with the desktop, the stack and HTTPS set above.
        env["LAZYOS_LAZYWEB"] = "1"
    env.update(desktop_app_env(cfg) | login_env(cfg) | script_env(cfg, SCRIPTS))  # Mail, tray demo; a script's own
    env.update(driver_env(cfg))  # LAZYOS_DEVD (issue #497)
    return env


def lazyrad_samples(user: str) -> str:
    """`LAZYRAD_SAMPLES`: the user's list, then the LazyOS samples it lacks."""
    entries = [entry for entry in user.split(os.pathsep) if entry]
    entries += [entry for entry in LAZYOS_LAZYRAD_SAMPLES if entry not in entries]
    return os.pathsep.join(entries)


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


def _script(cfg: dict) -> tuple:
    """The SCRIPTS entry selected by ``cfg["script"]``."""
    return SCRIPTS[cfg["script"]]


def build_plan(cfg: dict) -> list[dict]:
    """Turn a configuration dict into an ordered list of steps."""
    check_limits(cfg)
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
        if cfg.get("linuxapps") and not cfg["skip_build"]:
            # run_demo builds the programs and sets LAZYOS_LINUXAPPS itself.
            argv.append("--linuxapps")
        if cfg.get("tls") and not cfg["skip_build"]:
            # run_demo builds the HTTPS tools and sets LAZYOS_TLS itself.
            argv.append("--tls")
        if cfg.get("journal") and not cfg["skip_build"]:
            # run_demo sets LAZYOS_JOURNAL itself.
            argv.append("--journal")
        if cfg.get("lazyweb") and not cfg["skip_build"]:
            # run_demo builds the browser and sets the desktop, the stack,
            # HTTPS and LAZYOS_LAZYWEB itself.
            argv.append("--lazyweb")
        argv += desktop_app_argv(cfg) + login_argv(cfg)  # --mail, --traydemo; --autologin NAME (#623)
        if check_mode(cfg.get("display_mode", "")) and not cfg["skip_build"]:
            # run_demo sets LAZYOS_DISPLAY_MODE (`display.mode` in lazyos.cfg).
            argv += ["--display-mode", check_mode(cfg["display_mode"])]
        argv += assets_argv(cfg)  # run_demo sets LAZYOS_ASSETS (issue #454)
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
        # Recreate the OS volume (apps, settings, logs) instead of updating it; it needs a
        # build, so "Skip build" wins. run_demo asks before erasing and has no terminal
        # here, so the GUI asks first (datavol.confirm_reset_os) and passes --yes for it.
        if (cfg.get("reset_os") or cfg.get("setup")) and not cfg["skip_build"]:
            argv += ["--reset-os", "--yes"]
        argv += device_flags(cfg)  # sound card, NIC model, devd (`drivers`)
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
