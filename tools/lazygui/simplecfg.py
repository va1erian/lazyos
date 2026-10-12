"""The Simple tab's configuration: a build type, an interface and the extra
switches, turned into the full configuration :func:`catalog.build_plan`
takes (split out of `catalog.py`, which re-exports it).
"""

from __future__ import annotations

from .display import HIDPI_MODE
from .login import DEFAULT_ACCOUNT

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
    ("games", "Games", "LazyGolf, Doom, Quake, SRB2Kart (fetches Kart Krew's ~220 MB game data; "
                       "1G OS volume; never publish the image) and the LazyRAD brickbreaker.",
     ("doom", "quake", "srb2kart", "lazyrad")),
    ("multimedia", "Multimedia", "emusic music player and the LazyRAD MOD player.",
     ("emusic", "modplayer")),
    ("dev", "Dev", "The LazyRAD IDE and its samples, the Linux command-line programs "
                   "(dash, sqlite3, jq, rg) and the tray demo.",
     ("lazyrad", "linuxapps", "traydemo")),
    ("networking", "Networking", "Internet through QEMU, the Network and Net Tools apps, "
                                 "HTTPS with curl, wget and fetch.",
     ("net", "tls")),
    ("wifi", "Wi-Fi", "The Wi-Fi driver and firmware for the AMD NUC's RTL8822CE (turns "
                      "Networking on; QEMU has no such card).",
     ("wifi",)),
    ("web", "Web", "The LazyWeb browser, Mail and the LazySky Bluesky client "
                   "(turns Networking on).",
     ("lazyweb", "mail", "lazysky")),
)
#: The switches no group owns, still one checkbox each on the Simple tab.
SIMPLE_OPTIONS = ("shell", "devices", "hidpi", "autologin", "setup", "usb_image")


def group_switches(selected: set[str]) -> dict[str, bool]:
    """The per-app `simple_config` switches the groups in ``selected`` (keys of
    :data:`SIMPLE_GROUPS`) turn on; the Picture Viewer is always on."""
    on = {name for key, _l, _d, names in SIMPLE_GROUPS if key in selected for name in names}
    every = {name for _k, _l, _d, names in SIMPLE_GROUPS for name in names}
    return {name: name in on for name in sorted(every)} | {"pictures": True}


def simple_config(base: dict, build: str, interface: str, lazyrad: bool = False,
                  shell: bool = True, devices: bool = False, doom: bool = False,
                  quake: bool = False,
                  modplayer: bool = False, net: bool = False, linuxapps: bool = False,
                  hidpi: bool = False, tls: bool = False, lazyweb: bool = False,
                  mail: bool = False, traydemo: bool = False,
                  autologin: bool = False, setup: bool = False,
                  pictures: bool = False, emusic: bool = False,
                  usb_image: bool = False, wifi: bool = False, lazysky: bool = False,
                  srb2kart: bool = False) -> dict:
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
    at 2x (docs/hidpi-plan.md) and ``tls`` the HTTPS clients (curl, wget,
    fetch; it implies ``net``); ``lazyweb`` the LazyWeb browser (Desktop only;
    it implies ``tls``); ``mail`` the Mail app (Desktop only; it implies
    ``tls``); ``traydemo`` the tray sample app (Desktop only); ``autologin`` skips the
    Desktop's login screen and logs ``user`` in (issue #623); ``setup`` starts
    the Desktop with no account, so the login screen asks for its owner (the
    first-boot setup, docs/accounts-plan.md U1; it wins over ``autologin``); ``pictures``
    the Picture Viewer (Desktop only, docs/lazyrad-pictures.md); ``emusic`` adds
    the emusic package (Desktop only, like ``doom``); ``usb_image`` also writes
    ``target/lazyos-usb.img``, the real-PC stick image (Desktop only, docs/usb-stick.md); ``wifi``
    the Wi-Fi driver and its firmware (either interface; it implies ``net``);
    ``lazysky`` the LazySky Bluesky client (Desktop only, docs/lazysky.md; it implies
    ``net``). Machine settings
    (accelerator, memory, QEMU path) come from ``base``; every image switch is
    decided here so stale Advanced checkboxes cannot leak into a Simple boot.
    """
    if build not in dict(SIMPLE_BUILDS).values():
        raise ValueError(f"unknown build profile: {build!r}")
    if interface not in dict(SIMPLE_INTERFACES):
        raise ValueError(f"unknown interface: {interface!r}")
    desktop = interface == "Desktop"
    lazyweb = desktop and lazyweb
    lazysky = desktop and lazysky
    tls = tls or lazyweb or (desktop and mail)
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
        "net": net or tls or wifi or lazysky,
        "wifi": wifi,
        # The simulated radio is an Advanced-only test switch.
        "wifi_sim": False,
        "net_forwards": "",
        "net_restrict": False,
        "nics": "1",
        "linuxapps": linuxapps,
        "tls": tls,
        "journal": False,
        "lazyweb": lazyweb,
        "lazysky": lazysky,
        "mail": desktop and mail, "traydemo": desktop and traydemo,
        "pictures": desktop and pictures,
        "usb_image": desktop and usb_image,
        # The netboot payload is an Advanced-only control, like the kernel limits.
        "netboot": False,
        "netboot_key": "",
        "autologin": DEFAULT_ACCOUNT if desktop and autologin and not setup else "",
        "setup": desktop and setup,
        "display_mode": HIDPI_MODE if hidpi else "",
    })
    return cfg
