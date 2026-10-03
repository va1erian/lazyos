#!/usr/bin/env python3
"""Build LazyOS and boot the interactive CLI demo in QEMU — one command.

By default uses the **dev** profile (kernel at O2, dependencies at O3). Measured
in QEMU, that is the fastest configuration: the fully optimized **release**
profile (O3 + fat LTO) is *slower* under QEMU's TCG emulation for the
floating-point rasterizer, though it should win on real hardware. Pass
``--release`` to build it anyway.

Examples
--------
    python tools/run_demo.py                 # dev build (fast in QEMU) + boot
    python tools/run_demo.py --release       # optimized build for real hardware
    python tools/run_demo.py --no-build      # boot the existing target/lazyos.img
    python tools/run_demo.py -- --cpu max    # pass extra args to QEMU
    python tools/run_demo.py --reset-home    # wipe the home volume (target/home.img) first
    python tools/run_demo.py --reset-os      # wipe the OS volume too (asks first; LAZYOS_RESET_OS=1 build)
    python tools/run_demo.py --no-home-disk  # boot with only the boot disk
    python tools/run_demo.py --sound         # add a virtio-sound card (host speakers)
    python tools/run_demo.py --desktop --sound   # desktop session; type `beep` in the Terminal
    python tools/run_demo.py --desktop --no-shell  # desktop without LazyShell (bare compositor)
    python tools/run_demo.py --sound wav:out.wav   # ...recorded to a WAV file instead
    python tools/run_demo.py --doom          # desktop + /system/share/samples/doom.lzp
    python tools/run_demo.py --modplayer     # desktop + LazyRAD + /system/share/samples/modplayer.lzp, with sound
    python tools/run_demo.py --desktop --net # networking + the Network and Net Tools apps
    python tools/run_demo.py --net --net-forward 2323:2323   # also forward host 2323 (`nc -l 2323`)
    python tools/run_demo.py --linuxapps     # + dash, lua, sqlite3, jq, rg in /system/bin

The OS lives on an ext2 volume inside ``target/lazyos.img`` that ``cargo build``
updates in place (installed apps, settings and logs survive); ``--reset-os``
recreates it from scratch. A persistent ext2 home volume (default
``target/home.img``, label ``lazyhome``, 64 MiB) is attached as a second
virtio-blk device and mounted at ``/home``. It is created on first use and never
regenerated unless you pass ``--reset-home``. A fresh volume holds ``<user>/``
for the demo accounts (owned by them) and nothing else, so log in as ``user``
(password ``lazy``) or ``admin`` (password ``nimda``) to write to your own home. ``--data-disk PATH`` still attaches a legacy ext2
data volume (not mounted anywhere new); it is off by default.

In the demo: two windows run concurrently (a demo program and the `sh`
interpreter). Press Tab to move focus (green border); typed input goes to the
focused program.
"""

from __future__ import annotations

import argparse
import os
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent / "screenshot"))
from qemu_qmp import DEFAULT_MEMORY, accel_args, data_disk_args, find_qemu, home_disk_args  # noqa: E402

sys.path.insert(0, str(Path(__file__).resolve().parent))
import mkdisk  # noqa: E402
from lazygui.catalog import lazyrad_samples  # noqa: E402
from lazygui.display import add_display_options, build_display  # noqa: E402
from lazygui.limits import add_limit_option, build_limits  # noqa: E402
from demo_qemu import sound_args  # noqa: E402
from demo_builds import (  # noqa: E402
    build_doom, build_lazyrad, build_linuxapps, build_modplayer, build_rhai, build_xui_apps,
)

sys.path.insert(0, str(Path(__file__).resolve().parent / "abi"))
import busybox  # noqa: E402

sys.path.insert(0, str(Path(__file__).resolve().parent / "net"))
import qemu_net  # noqa: E402

ROOT = Path(__file__).resolve().parent.parent
DEFAULT_IMAGE = ROOT / "target" / "lazyos.img"
# LazyShell, the desktop shell (`tools/xui/build.py` output, issue #157).
XUI_SHELL = ROOT / "target" / "xui" / "xui-shell.elf"
# The desktop apps `--devices` opens at boot: the Terminal, then Devices.
DEVICES_AUTOSTART = "term,devices"
# The network apps a `--net` desktop ships (`build_support/xui_embed.rs`).
NET_APPS = [ROOT / "target" / "xui" / name for name in ("xui-network.elf", "xui-nettools.elf")]


def confirm(question: str) -> bool:
    """Ask on the terminal; anything but an explicit yes (or no TTY) is a no."""
    if not sys.stdin.isatty():
        return False
    try:
        return input(f"{question} [y/N] ").strip().lower() in ("y", "yes")
    except EOFError:  # e.g. stdin redirected from the null device
        return False


def prepare_data_disk(path: Path, reset: bool, assume_yes: bool) -> bool:
    """Make sure the legacy data volume exists, resetting it only when asked to."""
    return prepare_volume("data disk", path, reset, assume_yes, mkdisk.seeded,
                          mkdisk.DEFAULT_LABEL)


def prepare_home_disk(path: Path, reset: bool, assume_yes: bool) -> bool:
    """Make sure the home volume exists, resetting it only when asked to."""
    return prepare_volume("home disk", path, reset, assume_yes, mkdisk.home_volume,
                          mkdisk.HOME_LABEL)


def prepare_volume(what: str, path: Path, reset: bool, assume_yes: bool, plan,
                   label: str) -> bool:
    """Make sure a persistent volume exists, resetting it only when asked to.

    ``plan`` is a :class:`mkdisk.Layout`, or a callable returning one (so a
    layout that reads the accounts source fails inside the error handling).

    Returns ``False`` when the user declined an explicit reset or the volume
    could not be planned or written (reported on stderr, so the demo exits
    cleanly instead of with a traceback). Never regenerates an existing
    volume implicitly: that would destroy user data.
    """
    try:
        return _prepare_volume(what, path, reset, assume_yes, plan, label)
    except (OSError, ValueError) as error:
        # The plan reads the accounts source and the volume is a file on disk,
        # so either can legitimately fail (missing source, unwritable target).
        print(f"{what} {path}: {error}", file=sys.stderr)
        return False


def _prepare_volume(what: str, path: Path, reset: bool, assume_yes: bool, plan,
                    label: str) -> bool:
    exists = path.exists()
    if exists and not reset:
        return True  # nothing to create or reset: do not even read the plan
    layout = plan() if callable(plan) else plan  # only now, so a bad source cannot block a boot
    if reset and exists:
        question = (f"Erase {path} and format a fresh volume containing:\n"
                    f"{mkdisk.describe(layout)}\nProceed?")
        if not assume_yes and not confirm(question):
            print(f"{what} left untouched; aborting.", file=sys.stderr)
            return False
        mkdisk.format_image(path, label=label, layout=layout)
        print(f"reset {what}: {mkdisk.status(path).describe()}", flush=True)
    elif mkdisk.ensure_volume(path, label=label, layout=layout):
        print(f"created {what}: {mkdisk.status(path).describe()}", flush=True)
    return True


def build_xui_shell() -> bool:
    """Build the xui apps when LazyShell's binary is missing.

    The desktop profile embeds `target/xui/xui-shell.elf` (issue #157) and the
    image build fails without it, so a first `--desktop` run builds the apps
    here instead of failing; afterwards `python tools/xui/build.py` rebuilds
    them on demand, as before.
    """
    if XUI_SHELL.is_file():
        return True
    print("building the xui apps (LazyShell is missing)…", flush=True)
    result = subprocess.run([sys.executable, str(ROOT / "tools" / "xui" / "build.py")], cwd=ROOT)
    if result.returncode != 0 or not XUI_SHELL.is_file():
        print(f"{XUI_SHELL} was not built; pass --no-shell to boot the desktop without it",
              file=sys.stderr)
        return False
    return True


def build_core_packages() -> bool:
    """Package the built desktop apps (`tools/xui/core_packages.py`, issue
    #509): the desktop image embeds `target/pkg/core/*.lzp` in
    `/system/packages`. Cheap and reproducible (unchanged apps give the same
    archives, so `pkgd` does nothing at the next boot), so it runs before every
    desktop build; `tools/xui/build.py` runs it too."""
    script = ROOT / "tools" / "xui" / "core_packages.py"
    result = subprocess.run([sys.executable, str(script)], cwd=ROOT, stdout=subprocess.DEVNULL)
    if result.returncode != 0:
        print("error: the core packages did not build (run `python tools/xui/build.py`)",
              file=sys.stderr)
    return result.returncode == 0


def with_devices(autostart: str | None) -> str:
    """`LAZYOS_XUI_AUTOSTART` with the Devices app added: an existing list
    (`editor`) keeps its apps and gains `devices` once; no list means
    [`DEVICES_AUTOSTART`], the Terminal first. Mirrors `lazygui.catalog`."""
    if not autostart:
        return DEVICES_AUTOSTART
    if "devices" in [item.strip() for item in autostart.split(",")]:
        return autostart
    return f"{autostart},devices"


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument("--no-build", action="store_true", help="skip `cargo build`")
    parser.add_argument("--release", action="store_true",
                        help="build the optimized release profile (slower in QEMU)")
    parser.add_argument("--headless", action="store_true", help="no display window")
    parser.add_argument("--image", default=str(DEFAULT_IMAGE), help="disk image to boot")
    parser.add_argument("--qemu", help="path to qemu-system-x86_64")
    parser.add_argument("--memory", default=DEFAULT_MEMORY, help="guest RAM (default: %(default)s)")
    add_limit_option(parser)
    add_display_options(parser)
    parser.add_argument("--accel", default="auto",
                        choices=["auto", "none", "tcg", "whpx", "kvm"],
                        help="QEMU accelerator; auto uses whpx/kvm when available "
                             "(many times faster than TCG)")
    parser.add_argument("--disk", default="virtio", choices=["virtio", "ata"],
                        help="boot disk bus: virtio-blk (DMA, fast) or legacy IDE/ATA PIO")
    parser.add_argument("--home-disk", default=str(mkdisk.DEFAULT_HOME_PATH), metavar="PATH",
                        help="persistent ext2 home volume (label lazyhome, mounted at /home), "
                             "attached as a second virtio-blk device and created if missing "
                             "(default: %(default)s)")
    parser.add_argument("--no-home-disk", action="store_true",
                        help="do not attach a home volume (/home is then a directory on /)")
    parser.add_argument("--reset-home", action="store_true",
                        help="regenerate the home volume (asks first unless --yes)")
    parser.add_argument("--reset-os", action="store_true",
                        help="recreate the OS volume inside the image instead of updating it "
                             "in place: builds with LAZYOS_RESET_OS=1, which erases installed "
                             "apps, settings, logs and /data (home.img is not touched)")
    parser.add_argument("--data-disk", metavar="PATH", nargs="?",
                        const=str(mkdisk.DEFAULT_PATH),
                        help="also attach a legacy ext2 data volume as a virtio-blk device, "
                             "created if missing (off by default; no path means "
                             f"{mkdisk.DEFAULT_PATH}). Mounted nowhere new")
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
    parser.add_argument("--sound", nargs="?", const="auto", metavar="BACKEND",
                        help="attach a virtio-sound card and build with LAZYOS_SOUND=1, "
                             "which boots the `sndd` driver and plays its test tones. "
                             "BACKEND is a QEMU -audiodev driver (dsound, pa, alsa, sdl, "
                             "none, ...) or wav:PATH; default: this OS's usual one")
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
    parser.add_argument("--modplayer", action="store_true",
                        help="the desktop profile with LazyRAD, its MOD player sample at "
                             "/system/share/lazyrad/modplayer and the same app packaged at "
                             "/system/share/samples/modplayer.lzp (LAZYOS_LAZYRAD=1 "
                             "LAZYOS_MODPLAYER=1; builds it with tools/lazyrad/package.py) "
                             "and a sound card: copy it to your home and install it with "
                             "`pkgctl install`, or open it in Files, then start ModPlayer "
                             "from the menu")
    parser.add_argument("--linuxapps", action="store_true",
                        help="embed real Linux programs in /system/bin "
                             "(LAZYOS_LINUXAPPS=1): dash, lua, sqlite3, jq and rg, "
                             "built from pinned sources by tools/linuxapps/build.py")
    parser.add_argument("--devices", action="store_true",
                        help="the desktop profile with the Devices app open at boot "
                             "(devices, owners, rights and the driver class rules): "
                             "builds the xui apps, then LAZYOS_DESKTOP=1 and adds "
                             "`devices` to LAZYOS_XUI_AUTOSTART (default "
                             f"{DEVICES_AUTOSTART})")
    parser.add_argument("--no-rhai", action="store_true",
                        help="do not (re)build the `rhai` command before the image "
                             "(tools/rhai/build.py; incremental, so cheap when unchanged)")
    parser.add_argument("qemu_args", nargs=argparse.REMAINDER,
                        help="extra QEMU args (after `--`)")
    args = parser.parse_args(argv)
    # Samples are only embedded with the runtime that plays them, and the MOD
    # player is a LazyRAD app that wants speakers.
    args.lazyrad = args.lazyrad or bool(args.lazyrad_samples) or args.modplayer
    if args.modplayer and not args.sound:
        args.sound = "auto"
    # The Devices app and LazyRAD are desktop apps (LazyRAD is the core package
    # `os.lazy.lazyrad`, which only the desktop profile installs; the MOD player
    # brings LazyRAD): `--devices`, `--lazyrad` and `--modplayer` imply `--desktop`.
    args.desktop = args.desktop or args.devices or args.doom or args.lazyrad
    if args.no_data_disk and (args.reset_data or args.data_disk):
        parser.error("--no-data-disk conflicts with --data-disk / --reset-data")
    if args.no_home_disk and args.reset_home:
        parser.error("--reset-home conflicts with --no-home-disk")
    if args.reset_os and args.no_build:
        parser.error("--reset-os needs a build: it sets LAZYOS_RESET_OS=1 for `cargo build`")
    try:
        net_qemu, forwards = qemu_net.args_from_options(args)
        limits = build_limits(args.limit, args.no_build)
        limits.update(build_display(args))
    except ValueError as error:
        parser.error(str(error))

    if not args.no_build:
        cargo = ["cargo", "build"]
        profile = "dev (optimized deps; fastest in QEMU)"
        if args.release:
            cargo.append("--release")
            profile = "release (optimized for real hardware)"
        env = dict(os.environ)
        env.update(limits)
        if args.reset_os:
            if Path(args.image).exists() and not args.yes and not confirm(
                    f"Recreate the OS volume in {args.image}? Installed apps, settings, "
                    "logs and /data are erased.\nProceed?"):
                print("OS volume left untouched; aborting.", file=sys.stderr)
                return 1
            env["LAZYOS_RESET_OS"] = "1"
            print("--reset-os: the OS volume will be recreated "
                  "(apps, settings, logs and /data are erased)", flush=True)
        # The console shell (issue #254): cached after the first build, and a
        # git worktree reuses the main checkout's; `build.rs` warns if absent.
        busybox.ensure_busybox()
        if not args.no_rhai:
            build_rhai()
        if args.lazyrad:
            if not build_lazyrad():
                return 1
            env["LAZYOS_LAZYRAD"] = "1"
            # The caller's samples (--lazyrad-samples, else LAZYRAD_SAMPLES), then
            # the LazyOS-only ones (the Messenger demo).
            user = args.lazyrad_samples or os.environ.get("LAZYRAD_SAMPLES", "")
            env["LAZYRAD_SAMPLES"] = lazyrad_samples(user)
        if args.doom:
            if not build_doom():
                return 1
            env["LAZYOS_DOOM"] = "1"
        if args.modplayer:
            # After build_lazyrad: the package carries the player it just built.
            if not build_modplayer():
                return 1
            env["LAZYOS_MODPLAYER"] = "1"
        if args.linuxapps:
            if not build_linuxapps():
                return 1
            env["LAZYOS_LINUXAPPS"] = "1"
        print(f"building LazyOS [{profile}]…", flush=True)
        if args.sound:
            env["LAZYOS_SOUND"] = "1"
        if args.net:
            # The whole stack (it implies the driver). `demo=0`: an interactive
            # boot runs `netd` without the harness's evidence clients, which
            # talk to host servers only `tools/net/run.py` starts.
            env["LAZYOS_NETD"] = "1"
            env.setdefault("LAZYOS_NETD_ARGS", "demo=0")
            if args.desktop and not all(app.is_file() for app in NET_APPS)                     and not build_xui_apps():
                return 1
        if args.desktop:
            env["LAZYOS_DESKTOP"] = "1"
        if args.devices:
            if not build_xui_apps():
                return 1
            env["LAZYOS_XUI_AUTOSTART"] = with_devices(env.get("LAZYOS_XUI_AUTOSTART"))
        if args.no_shell:
            env["LAZYOS_SHELL"] = "0"
        elif args.desktop and env.get("LAZYOS_SHELL") != "0" and not build_xui_shell():
            return 1
        if args.desktop and not build_core_packages():
            return 1
        result = subprocess.run(cargo, cwd=ROOT, env=env)
        if result.returncode != 0:
            return result.returncode

    image = Path(args.image)
    if not image.is_file():
        print(f"disk image not found: {image}\nRun without --no-build to build it.", file=sys.stderr)
        return 1

    home_disk = None if args.no_home_disk else Path(args.home_disk)
    if home_disk and not prepare_home_disk(home_disk, args.reset_home, args.yes):
        return 1
    data_disk = None
    if not args.no_data_disk and (args.data_disk or args.reset_data):
        data_disk = Path(args.data_disk or mkdisk.DEFAULT_PATH)
        if not prepare_data_disk(data_disk, args.reset_data, args.yes):
            return 1

    qemu = find_qemu(args.qemu)
    command = [
        qemu,
        "-m", args.memory,
        "-device", "isa-debug-exit,iobase=0xf4,iosize=0x04",
        "-serial", "mon:stdio",
    ]
    # virtio-blk is DMA-based; the IDE/PIO path costs a VM exit per 16 bits read,
    # which made loading the ~2.7 MB desktop ELFs take tens of seconds.
    if args.disk == "virtio":
        command += ["-drive", f"format=raw,file={image},if=none,id=boot",
                    "-device", "virtio-blk-pci,drive=boot"]
    else:
        command += ["-drive", f"format=raw,file={image}"]
    # Same order as the screenshot tools: boot, data, then home.
    if data_disk:
        command += data_disk_args(data_disk)
    if home_disk:
        command += home_disk_args(home_disk)
    if args.sound:
        command += sound_args(args.sound)
    if args.net:
        busy = qemu_net.busy_ports(forwards)
        if busy:
            print(f"host port(s) {', '.join(busy)} already in use; pick another with "
                  "--net-forward HOSTPORT:GUESTPORT (or --net-forward none)", file=sys.stderr)
            return 1
        command += net_qemu
        print(qemu_net.describe(forwards, args.net_restrict), flush=True)
    command += accel_args(args.accel, qemu)
    if args.headless:
        command += ["-display", "none"]

    extra = args.qemu_args
    if extra and extra[0] == "--":
        extra = extra[1:]
    command += extra

    print("running:", " ".join(command), flush=True)
    return subprocess.call(command)


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
