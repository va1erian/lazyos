"""The Simple tab's configuration: a build type, an interface and the extra
switches, turned into the full configuration :func:`catalog.build_plan`
takes (split out of `catalog.py`, which re-exports it).
"""

from __future__ import annotations

import inspect
import os
import sys

from .display import HIDPI_MODE, HIRES_SCALE
from .login import DEFAULT_ACCOUNT

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
import suites  # noqa: E402

#: The Simple tab's Software choices: ("", the groups below decide), then each suite.
SOFTWARE_CUSTOM = "Custom"

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

#: The Simple tab's feature groups: (key, label, description, the `simple_config`
#: switches it turns on). A group is one checkbox; Desktop-only switches mean
#: nothing on the CLI (`simple_config` drops them there). LazyRAD comes with
#: Games (the brickbreaker sample and the player), Multimedia (the MOD player is
#: a LazyRAD app) and Dev; the Picture Viewer is always in a Desktop image.
SIMPLE_GROUPS = (
    ("games", "Games", "LazyGolf, the NES Player, Doom, Quake, SRB2Kart (fetches Kart Krew's ~220 MB game data; "
                       "1G OS volume, 1.5G beside a browser; never publish the image) and the LazyRAD brickbreaker.",
     ("games", "doom", "quake", "srb2kart", "lazyrad")),
    ("multimedia", "Multimedia", "emusic music player and the LazyRAD MOD player.",
     ("emusic", "modplayer")),
    ("dev", "Dev", "The LazyRAD IDE and its samples, the Linux command-line programs "
                   "(dash, sqlite3, jq, rg) and the tray demo.",
     ("lazyrad", "linuxapps", "traydemo")),
    ("networking", "Networking", "Internet through QEMU, the Network and Net Tools apps, "
                                 "HTTPS with curl, wget and fetch.",
     ("net", "tls")),
    ("wifi", "Wi-Fi", "The Wi-Fi driver and firmware for the RTL8822CE (turns "
                      "Networking on; QEMU has no such card).",
     ("wifi",)),
    ("web", "Web", "Mail and the LazySky Bluesky client "
                   "(turns Networking on).",
     ("mail", "lazysky")),
    ("devtools", "Developer tools", "The Messenger Playground: browse Messenger APIs and topics and script "
                                    "them in Rhai. A developer tool runs with your own authority, like "
                                    "the Terminal; builds the rhai command it runs scripts with (Desktop only).",
     ("devtools",)),
    ("lazywebplus", "LazyWeb+", "The web browser, on the WebKit engine: JavaScript and the "
                                "modern web, with history, downloads and an ad blocker; "
                                "three programs of about 108 MB, 4 GiB of RAM and a 1 GiB OS "
                                "volume; builds in Docker (turns Networking on).",
     ("lazywebplus",)),
)
#: The switches no group owns, still one checkbox each on the Simple tab.
SIMPLE_OPTIONS = ("shell", "devices", "hidpi", "hires", "autologin", "setup", "usb_image",
                  "usb_audio")


def software_choices() -> list[str]:
    """The Simple tab's Software radio labels: Custom, then each suite, `base` first."""
    return [SOFTWARE_CUSTOM, *(name.capitalize() for name in suites.suite_names())]


def suite_name(label: str) -> str:
    """The suite a Software label names (``""`` for Custom)."""
    return "" if label == SOFTWARE_CUSTOM else label.lower()


def suite_argv(cfg: dict) -> list[str]:
    """`--suite NAME [--with srb2kart]` for `run_demo.py` (none without a build:
    "Skip build" boots the image as it is)."""
    if not cfg.get("suite") or cfg.get("skip_build"):
        return []
    return ["--suite", cfg["suite"], *(["--with", "srb2kart"] if cfg.get("suite_srb2kart") else [])]


def simple_config(base: dict, build: str, interface: str, suite: str = "",
                  suite_srb2kart: bool = False, **switches: bool) -> dict:
    """:func:`plain_config` for a Simple-mode choice, with a software suite on top
    (`tools/suites.toml`): its flags are turned on beside the switches given, and
    the ones the Simple tab has no checkbox for (`smb`, the big-app profile) go
    straight into the configuration. A suite is Desktop only, as its apps are."""
    extra: dict[str, bool] = {}
    if suite and interface == "Desktop":
        known = inspect.signature(plain_config).parameters
        for flag in suites.resolve(suite, ["srb2kart"] if suite_srb2kart else []):
            name = suites.dest(flag)
            if name in known:
                switches[name] = True
            elif name != "desktop":
                extra[name] = True
    cfg = plain_config(base, build, interface, **switches)
    cfg.update(extra)
    if suite and interface == "Desktop":
        cfg.update({"suite": suite, "suite_srb2kart": suite_srb2kart})
    return cfg


def group_switches(selected: set[str]) -> dict[str, bool]:
    """The per-app `simple_config` switches the groups in ``selected`` (keys of
    :data:`SIMPLE_GROUPS`) turn on; the Picture Viewer is always on."""
    on = {name for key, _l, _d, names in SIMPLE_GROUPS if key in selected for name in names}
    every = {name for _k, _l, _d, names in SIMPLE_GROUPS for name in names}
    return {name: name in on for name in sorted(every)} | {"pictures": True}


def plain_config(base: dict, build: str, interface: str, lazyrad: bool = False,
                  shell: bool = True, devices: bool = False, doom: bool = False,
                  quake: bool = False,
                  modplayer: bool = False, net: bool = False, linuxapps: bool = False,
                  hidpi: bool = False, hires: bool = False, tls: bool = False,
                  mail: bool = False, traydemo: bool = False,
                  autologin: bool = False, setup: bool = False,
                  pictures: bool = False, emusic: bool = False,
                  usb_image: bool = False, wifi: bool = False, lazysky: bool = False,
                  srb2kart: bool = False,
                  lazywebplus: bool = False, games: bool = False, devtools: bool = False,
                  usb_audio: bool = False) -> dict:
    """The full configuration for a Simple-mode choice.

    ``build`` is a cargo profile (``dev``/``release``) and ``interface`` is
    ``CLI`` or ``Desktop``; ``lazyrad`` adds the LazyRAD IDE to a Desktop
    image (a core package like the other desktop apps, so it means nothing on
    the CLI), ``shell`` keeps the LazyShell desktop (taskbar, start menu) on
    it, ``devices`` opens the Devices app at boot, ``doom`` adds the Doom
    package and ``modplayer`` the LazyRAD MOD player package (likewise Desktop
    only); ``net`` adds networking to either interface (the stack, QEMU's user
    network with host port 8080 forwarded, and on the desktop the Network and
    Net Tools apps), ``linuxapps`` the Linux command-line programs (dash,
    sqlite3, jq, rg), ``hidpi`` a 2560x1440 screen showing a 1280x720 desktop
    at 2x (docs/hidpi-plan.md), ``hires`` that screen as a roomy 2560x1440
    desktop at 1x (text keeps its size; it excludes ``hidpi``) and ``tls`` the HTTPS clients (curl, wget,
    fetch; it implies ``net``); ``mail`` the Mail app (Desktop only; it implies
    ``tls``); ``traydemo`` the tray sample app (Desktop only); ``games`` LazyGolf and the
    NES Player (Desktop only; the Games group); ``devtools`` the developer tools (the Messenger
    Playground; Desktop only, docs/devtool-packages-plan.md); ``autologin`` skips the
    Desktop's login screen and logs ``user`` in (issue #623); ``setup`` starts
    the Desktop with no account, so the login screen asks for its owner (the
    first-boot setup, docs/accounts-plan.md U1; it wins over ``autologin``); ``pictures``
    the Picture Viewer (Desktop only, docs/lazyrad-pictures.md); ``emusic`` adds
    the emusic package (Desktop only, like ``doom``); ``usb_image`` also writes
    ``target/lazyos-usb.img``, the real-PC stick image (Desktop only, docs/usb-stick.md);
    ``usb_audio`` USB speakers, headsets and DACs as a second output (either interface,
    docs/usb-audio-plan.md); ``wifi``
    the Wi-Fi driver and its firmware (either interface; it implies ``net``);
    ``lazysky`` the LazySky Bluesky client (Desktop only, docs/lazysky.md; it implies
    ``net``); ``lazywebplus`` LazyWeb+, the web browser on WebKit (Desktop only, docs/lazywebplus.md; it
    implies ``net`` and the big-app profile).
    Machine settings
    (accelerator, memory, QEMU path) come from ``base``; every image switch is
    decided here so stale Advanced checkboxes cannot leak into a Simple boot.
    """
    if build not in dict(SIMPLE_BUILDS).values():
        raise ValueError(f"unknown build profile: {build!r}")
    if interface not in dict(SIMPLE_INTERFACES):
        raise ValueError(f"unknown interface: {interface!r}")
    if hidpi and hires:
        raise ValueError("HiDPI (a 1280x720 desktop at 2x) and Hi-res (a 2560x1440 desktop at 1x) "
                         "exclude each other")
    desktop = interface == "Desktop"
    lazysky = desktop and lazysky
    lazywebplus = desktop and lazywebplus
    # LazyWeb+'s ad blocker updates its lists with `curl` (issue #845).
    tls = tls or lazywebplus or (desktop and mail)
    cfg = dict(base)
    cfg.update({
        "mode": "Interactive demo",
        "profile": build,
        "skip_build": False,
        # Kernel limits are an Advanced-only control: never carried into Simple.
        "limits": "",
        "headless": False,
        "extra": base.get("extra", ""),
        "busybox": "",
        "cli": not desktop,
        # The desktop gets a sound card (type `beep` in the Terminal); the CLI
        # image stays quiet, since a sound card there means boot-time test tones.
        "sound": desktop,
        # Desktop = the single `LAZYOS_DESKTOP=1` profile (issue #217): services, compositor,
        # the xui apps as its clients (nothing opens at boot), no demo/evidence programs.
        # The individual switches stay off so no Advanced checkbox leaks in.
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
        "quake": desktop and quake,
        "srb2kart": desktop and srb2kart,
        "emusic": desktop and emusic,
        "modplayer": desktop and modplayer,
        "net": net or tls or wifi or lazysky or lazywebplus,
        "wifi": wifi,
        # The simulated radio is an Advanced-only test switch.
        "wifi_sim": False,
        "net_forwards": "",
        "net_restrict": False,
        "nics": "1",
        "linuxapps": linuxapps,
        "tls": tls,
        "journal": False,
        "lazysky": lazysky,
        "lazywebplus": lazywebplus,
        "mail": desktop and mail, "traydemo": desktop and traydemo,
        "games": desktop and games,
        "devtools": desktop and devtools,
        "pictures": desktop and pictures,
        "usb_image": desktop and usb_image,
        "usb_audio": usb_audio,
        # The netboot payload is an Advanced-only control, like the kernel limits.
        "netboot": False,
        "netboot_key": "",
        "autologin": DEFAULT_ACCOUNT if desktop and autologin and not setup else "",
        "setup": desktop and setup,
        "display_mode": HIDPI_MODE if hidpi or hires else "",
        # Scale 1 on the 1440p screen is the roomy desktop; HiDPI leaves the image's auto (2).
        "display_scale": HIRES_SCALE if hires else "",
    })
    return cfg
