"""The command line of `tools/run_demo.py`: its flags and what they imply."""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent / "screenshot"))
from qemu_qmp import DEFAULT_MEMORY  # noqa: E402

sys.path.insert(0, str(Path(__file__).resolve().parent))
import mkdisk  # noqa: E402
from lazygui.assets import add_assets_option, build_assets  # noqa: E402
from lazygui.display import add_display_options, build_display  # noqa: E402
from lazygui.limits import add_limit_option, build_limits  # noqa: E402
from lazygui.login import add_login_option, build_login  # noqa: E402

sys.path.insert(0, str(Path(__file__).resolve().parent / "net"))
import qemu_net  # noqa: E402
from demo_qemu import add_device_options  # noqa: E402

# The desktop apps `--devices` opens at boot when no list is set: just
# Devices, since the desktop opens nothing at boot by default.
DEVICES_AUTOSTART = "devices"


def make_parser(description: str, default_image: Path) -> argparse.ArgumentParser:
    """Every `run_demo.py` flag; `description` is its module docstring."""
    parser = argparse.ArgumentParser(
        description=description, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument("--no-build", action="store_true", help="skip `cargo build`")
    parser.add_argument("--build-only", action="store_true",
                        help="build the image(s) and exit without booting QEMU")
    parser.add_argument("--release", action="store_true",
                        help="build the optimized release profile (slower in QEMU)")
    parser.add_argument("--headless", action="store_true", help="no display window")
    parser.add_argument("--image", default=str(default_image), help="disk image to boot")
    parser.add_argument("--qemu", help="path to qemu-system-x86_64")
    parser.add_argument("--memory", default=DEFAULT_MEMORY, help="guest RAM (default: %(default)s)")
    add_limit_option(parser)
    add_display_options(parser)
    add_assets_option(parser)
    parser.add_argument("--accel", default="auto",
                        choices=["auto", "none", "tcg", "whpx", "kvm"],
                        help="QEMU accelerator; auto uses whpx/kvm when available "
                             "(many times faster than TCG)")
    parser.add_argument("--disk", default="virtio", choices=["virtio", "ata", "ahci"],
                        help="boot disk bus: virtio-blk (DMA, fast), legacy IDE/ATA PIO, "
                             "or QEMU's AHCI (SATA) controller (docs/ahci-plan.md A3)")
    parser.add_argument("--home-disk", default=str(mkdisk.DEFAULT_HOME_PATH), metavar="PATH",
                        help="persistent ext2 home volume (label lazyhome, mounted at /home), "
                             "attached as a second virtio-blk device and created if missing "
                             "(default: %(default)s)")
    parser.add_argument("--no-home-disk", action="store_true",
                        help="do not attach a home volume (/home is then a directory on /)")
    parser.add_argument("--reset-home", action="store_true",
                        help="regenerate the home volume (asks first unless --yes)")
    parser.add_argument("--reset-os", action="store_true",
                        help="recreate the OS volume instead of updating it in place "
                             "(LAZYOS_RESET_OS=1): erases apps, settings, logs, not home.img")
    parser.add_argument("--data-disk", metavar="PATH", nargs="?", const=str(mkdisk.DEFAULT_PATH),
                        help="also attach a legacy ext2 data volume, created if missing "
                             f"(default path {mkdisk.DEFAULT_PATH}; mounted nowhere new)")
    parser.add_argument("--no-data-disk", action="store_true",
                        help="do not attach a data volume (the default; kept for older callers)")
    parser.add_argument("--reset-data", action="store_true",
                        help="regenerate the data volume with the seeded layout "
                             "(asks first unless --yes; attaches it if --data-disk is unset)")
    parser.add_argument("--yes", "-y", action="store_true",
                        help="answer yes to the --reset-home / --reset-data / --reset-os "
                             "confirmation")
    parser.add_argument("--desktop", action="store_true",
                        help="build the desktop profile (LAZYOS_DESKTOP=1; needs the xui apps "
                             "from `python tools/xui/build.py`)")
    parser.add_argument("--no-shell", action="store_true",
                        help="with --desktop, leave LazyShell (taskbar, start menu, desktop "
                             "icons) out of the image (LAZYOS_SHELL=0): the compositor then "
                             "shows background and windows only")
    add_login_option(parser)  # --autologin NAME (LAZYOS_AUTOLOGIN, issue #623)
    add_device_options(parser)  # --sound, --sound-card, --nic, --no-devd
    qemu_net.add_net_options(
        parser,
        "attach a virtio-net card on QEMU's user-mode network and build the network "
        "stack (LAZYOS_NETD=1: the `netdrv` driver, `netd` with DHCP, `ping`, `nslookup`, "
        "`nc` and `ftp` in the shell, and on the desktop the Network and Net Tools apps). "
        f"Host port {qemu_net.NETTOOLS_PORT} is forwarded to the guest's (Net Tools' web "
        "server); see docs/networking-host-access.md. The packet-capture-judged run is "
        "`python tools/net/run.py`")
    parser.add_argument("--lazyrad", action="store_true",
                        help="build the LazyRAD IDE and player and ship them as the core "
                             "package os.lazy.lazyrad (LAZYOS_LAZYRAD=1); with --desktop "
                             "pkgd installs it at boot and Settings -> Menu offers it")
    parser.add_argument("--lazyrad-samples", metavar="DIRS",
                        help="sample project directories to copy under "
                             "/system/share/lazyrad/ (LAZYRAD_SAMPLES; `;` on Windows, "
                             "`:` elsewhere), e.g. <lazyrad>/examples/hello; the "
                             "lazyrad_*.json sessions need them. Implies --lazyrad")
    parser.add_argument("--doom", action="store_true",
                        help="the desktop profile with the Doom package at "
                             "/system/share/samples/doom.lzp (LAZYOS_DOOM=1; builds it "
                             "with tools/doom/build.py, which fetches doomgeneric and "
                             "Freedoom): install it with `pkgctl install "
                             "/system/share/samples/doom.lzp` or by opening it in Files, "
                             "then start Doom from the menu")
    parser.add_argument("--emusic", action="store_true",
                        help="the desktop profile with the emusic package at "
                             "/system/share/samples/emusic.lzp (LAZYOS_EMUSIC=1; builds it "
                             "with tools/emusic/build.py, which fetches emusic) and a sound "
                             "card: copy it to your home and install it with `pkgctl "
                             "install`, or open it in Files, then start emusic from the menu")
    parser.add_argument("--modplayer", action="store_true",
                        help="the desktop profile with LazyRAD, its MOD player sample at "
                             "/system/share/lazyrad/modplayer and the same app packaged at "
                             "/system/share/samples/modplayer.lzp (LAZYOS_LAZYRAD=1 "
                             "LAZYOS_MODPLAYER=1; builds it with tools/lazyrad/package.py) "
                             "and a sound card: copy it to your home and install it with "
                             "`pkgctl install`, or open it in Files, then start ModPlayer "
                             "from the menu")
    parser.add_argument("--usb-image", action="store_true",
                        help="also write target/lazyos-usb.img, the image for a real PC's "
                             "USB stick (LAZYOS_USB_IMAGE=1 LAZYOS_USB=1, a services session; docs/usb-stick.md); the run "
                             "still boots target/lazyos.img (tools/boot/run.py boots the stick)")
    parser.add_argument("--linuxapps", action="store_true",
                        help="embed real Linux programs in /system/bin "
                             "(LAZYOS_LINUXAPPS=1): dash, lua, sqlite3, jq and rg, "
                             "built from pinned sources by tools/linuxapps/build.py")
    parser.add_argument("--tls", action="store_true",
                        help="networking plus the HTTPS clients (LAZYOS_TLS=1): `curl`, "
                             "`wget` and `fetch` in /system/bin, one rustls program that "
                             "verifies certificates against /etc/ssl/certs, built by "
                             "tools/nettls/build.py (docs/tls-plan.md)")
    parser.add_argument("--smb", action="store_true",
                        help="networking plus the SMB 2.1 client `smb` (LAZYOS_SMB=1): "
                             "`smb -U USER //SERVER/SHARE ls ; get FILE ! ; put -g N FILE` "
                             "(docs/smb-plan.md F2)")
    parser.add_argument("--dbgd", action="store_true",
                        help="networking plus `dbgd`, the remote inspection service "
                             "(LAZYOS_DBGD=1, docs/dbgd-plan.md): log, tasks, devices, USB and "
                             "Messenger state as JSON-RPC on guest port 9701, forwarded to host "
                             "9701 and guarded by the key in target/dbgd.key; read it with "
                             "`python tools/dbg/dbgctl.py`. LAZYOS_DBGD_KEY/_PORT/_PEER set the "
                             "key, port and the one peer address")
    parser.add_argument("--journal", nargs="?", const="1", metavar="BLOCKS",
                        help="give the OS volume an ext2 journal (LAZYOS_JOURNAL): metadata "
                             "commits are logged and replayed after a crash, so an unclean "
                             "stop needs no repair (docs/architecture/journal.md). BLOCKS "
                             "sets the log size; default 4096 blocks (16 MiB). An existing "
                             "image gets one on the next in-place update")
    parser.add_argument("--lazyweb", action="store_true",
                        help="the desktop profile with the LazyWeb browser (LAZYOS_LAZYWEB=1, "
                             "a core package; NetSurf compiled with zig by tools/xui/build.py), "
                             "networking and the HTTPS tools (docs/lazyweb.md)")
    parser.add_argument("--mail", action="store_true",
                        help="the desktop with HTTPS and Mail, esMail's IMAP/SMTP client "
                             "(LAZYOS_MAIL=1, docs/mail.md)")
    parser.add_argument("--pictures", action="store_true",
                        help="the desktop with the Picture Viewer, a LazyRAD app that opens "
                             "PNG, JPEG, BMP and GIF pictures (LAZYOS_PICTURES=1, a core "
                             "package; builds the LazyRAD player with tools/lazyrad/build.py; "
                             "docs/lazyrad-pictures.md)")
    parser.add_argument("--traydemo", action="store_true",
                        help="the desktop with the tray sample app os.lazy.traydemo "
                             "(LAZYOS_TRAYDEMO=1, docs/tray-plan.md)")
    parser.add_argument("--devices", action="store_true",
                        help="the desktop profile with the Devices app open at boot "
                             "(devices, owners, rights and the driver class rules): "
                             "builds the xui apps, then LAZYOS_DESKTOP=1 and adds "
                             "`devices` to LAZYOS_XUI_AUTOSTART (unset: "
                             f"{DEVICES_AUTOSTART}; set LAZYOS_XUI_AUTOSTART=term,devices "
                             "to open the Terminal too)")
    parser.add_argument("--timer", choices=["pit", "lapic"],
                        help="tick source test switch (LAZYOS_TIMER): `lapic` uses the "
                             "local APIC timer even where the PIT ticks, the path a PC "
                             "with a clock-gated PIT takes (docs/real-pc-boot-plan.md H2)")
    parser.add_argument("--no-rhai", action="store_true",
                        help="do not (re)build the `rhai` command before the image "
                             "(tools/rhai/build.py; incremental, so cheap when unchanged)")
    parser.add_argument("qemu_args", nargs=argparse.REMAINDER,
                        help="extra QEMU args (after `--`)")
    return parser


def parse_args(parser: argparse.ArgumentParser, argv: list[str]):
    """Parse `argv`, apply the flags' implications and check their conflicts.

    Returns `(args, net_qemu, forwards, limits)`.
    """
    args = parser.parse_args(argv)
    # Samples are only embedded with the runtime that plays them, and the MOD
    # player is a LazyRAD app that wants speakers.
    args.lazyrad = args.lazyrad or bool(args.lazyrad_samples) or args.modplayer
    if (args.modplayer or args.emusic) and not args.sound:
        args.sound = "auto"
    # The Devices app and LazyRAD are desktop apps (LazyRAD is the core package
    # `os.lazy.lazyrad`, which only the desktop profile installs; the MOD player
    # brings LazyRAD): `--devices`, `--lazyrad` and `--modplayer` imply `--desktop`,
    # as do the desktop-only apps (Doom, LazyWeb, Mail, the Picture Viewer, the
    # tray demo) and the
    # first-boot setup, which is the desktop login screen's (`--setup`).
    args.desktop = (args.desktop or args.devices or args.doom or args.emusic or args.lazyrad
                    or args.lazyweb or args.mail or args.pictures or args.traydemo or args.setup)
    # A browser wants HTTPS (curl too), Mail speaks TLS, and HTTPS needs a network.
    args.tls = args.tls or args.lazyweb or args.mail
    args.net = args.net or args.tls or args.smb or args.dbgd
    if args.dbgd and not args.net_forward:
        # The default forwards plus dbgd's port, so `dbgctl` works at once.
        args.net_forward = list(qemu_net.DEFAULT_FORWARDS) + ["9701:9701"]
    if args.no_data_disk and (args.reset_data or args.data_disk):
        parser.error("--no-data-disk conflicts with --data-disk / --reset-data")
    if args.no_home_disk and args.reset_home:
        parser.error("--reset-home conflicts with --no-home-disk")
    if args.build_only and args.no_build:
        parser.error("--build-only conflicts with --no-build")
    if args.reset_os and args.no_build:
        parser.error("--reset-os needs a build: it sets LAZYOS_RESET_OS=1 for `cargo build`")
    try:
        net_qemu, forwards = qemu_net.args_from_options(args)
        limits = build_limits(args.limit, args.no_build)
        limits.update(build_display(args))
        limits.update(build_assets(args))
        limits.update(build_login(args))
    except ValueError as error:
        parser.error(str(error))
    return args, net_qemu, forwards, limits
