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
    python tools/run_demo.py --desktop --autologin user  # skip the login screen (LAZYOS_AUTOLOGIN)
    python tools/run_demo.py --sound wav:out.wav   # ...recorded to a WAV file instead
    python tools/run_demo.py --doom          # desktop + /system/share/samples/doom.lzp
    python tools/run_demo.py --modplayer     # desktop + LazyRAD + /system/share/samples/modplayer.lzp, with sound
    python tools/run_demo.py --desktop --net # networking + the Network and Net Tools apps
    python tools/run_demo.py --net --net-forward 2323:2323   # also forward host 2323 (`nc -l 2323`)
    python tools/run_demo.py --linuxapps     # + dash, lua, sqlite3, jq, rg in /system/bin
    python tools/run_demo.py --tls           # networking + curl/wget/fetch over HTTPS
    python tools/run_demo.py --journal       # the OS volume gets an ext2 journal (LAZYOS_JOURNAL=1)
    python tools/run_demo.py --lazyweb       # desktop + networking + HTTPS + the LazyWeb browser
    python tools/run_demo.py --mail          # desktop + HTTPS + the Mail app (esMail; docs/mail.md)
    python tools/run_demo.py --assets ~/mods # + ~/mods (with its manifest.txt) in /system/share

The OS lives on an ext2 volume inside ``target/lazyos.img`` that ``cargo build``
updates in place (installed apps, settings and logs survive); ``--reset-os``
recreates it from scratch. A persistent ext2 home volume (default
``target/home.img``, label ``lazyhome``, 64 MiB) is attached as a second
virtio-blk device and mounted at ``/home``. It is created on first use and never
regenerated unless you pass ``--reset-home``. A fresh volume holds ``<user>/``
for the demo accounts (owned by them) and nothing else, so log in as ``user``
(password ``lazy``) or ``admin`` (password ``nimda``) to write to your own home.
The desktop starts at its login screen and runs everything as the account that
logged in (issue #623); ``--autologin NAME`` logs that account straight in.

In the demo: two windows run concurrently (a demo program and the `sh`
interpreter). Press Tab to move focus (green border); typed input goes to the
focused program.
"""

from __future__ import annotations

import os
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent / "screenshot"))
from qemu_qmp import accel_args, data_disk_args, find_qemu, home_disk_args  # noqa: E402

sys.path.insert(0, str(Path(__file__).resolve().parent))
import mkdisk  # noqa: E402
from lazygui.catalog import lazyrad_samples  # noqa: E402
from demo_qemu import device_env, sound_args  # noqa: E402
import demo_builds  # noqa: E402,F401  (tests patch its paths)
from demo_builds import (  # noqa: E402
    build_doom, build_lazyrad, build_lazyweb, build_linuxapps, build_mail, build_modplayer,
    build_rhai, build_tls, build_xui_apps,
)
from demo_args import DEVICES_AUTOSTART, make_parser, parse_args  # noqa: E402

sys.path.insert(0, str(Path(__file__).resolve().parent / "abi"))
import busybox  # noqa: E402

sys.path.insert(0, str(Path(__file__).resolve().parent / "net"))
import qemu_net  # noqa: E402

ROOT = Path(__file__).resolve().parent.parent
DEFAULT_IMAGE = ROOT / "target" / "lazyos.img"
# LazyShell, the desktop shell (`tools/xui/build.py` output, issue #157).
XUI_SHELL = ROOT / "target" / "xui" / "xui-shell.elf"
# The apps every desktop image ships (`build_support/xui_embed.rs`
# `DESKTOP_XUI_APPS` and `DOCUMENT_XUI_APPS`): the image build fails when one
# is missing, so a `target/xui` built before an app was added (LazyWriter,
# issue #533) is rebuilt first.
DESKTOP_ELFS = [ROOT / "target" / "xui" / name for name in (
    "xui-term.elf", "xui-sysmon.elf", "xui-fabricmon.elf", "xui-widget.elf", "xui-counter.elf",
    "xui-editor.elf", "xui-files.elf", "xui-paint.elf", "xui-writer.elf", "xui-archiver.elf",
    "xui-settings.elf", "xui-confd.elf", "xui-installer.elf", "xui-devices.elf",
    "xui-calc.elf",
    "xui-pdf.elf",
)]
# The network apps and print spooler a `--net` desktop ships (`build_support/xui_embed.rs`).
NET_APPS = [ROOT / "target" / "xui" / n for n in ("xui-network.elf", "xui-nettools.elf", "xui-printd.elf")]


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
    [`DEVICES_AUTOSTART`] alone. Mirrors `lazygui.catalog`."""
    if not autostart:
        return DEVICES_AUTOSTART
    if "devices" in [item.strip() for item in autostart.split(",")]:
        return autostart
    return f"{autostart},devices"


def main(argv: list[str]) -> int:
    parser = make_parser(__doc__, DEFAULT_IMAGE)
    args, net_qemu, forwards, limits = parse_args(parser, argv)

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
        # Opt-in apps, built before their switch (the MOD player after LazyRAD's).
        for wanted, build, switch in ((args.doom, build_doom, "LAZYOS_DOOM"),
                                      (args.modplayer, build_modplayer, "LAZYOS_MODPLAYER"),
                                      (args.linuxapps, build_linuxapps, "LAZYOS_LINUXAPPS"),
                                      (args.tls, build_tls, "LAZYOS_TLS"),
                                      (args.lazyweb, build_lazyweb, "LAZYOS_LAZYWEB")):
            if not wanted:
                continue
            if not build():
                return 1
            env[switch] = "1"
        if args.journal:
            env["LAZYOS_JOURNAL"] = args.journal
        print(f"building LazyOS [{profile}]…", flush=True)
        if args.sound:
            env["LAZYOS_SOUND"] = "1"
        device_env(args, env)
        if args.net:
            # The whole stack (it implies the driver). `demo=0`: an interactive
            # boot runs `netd` without the harness's evidence clients, which
            # talk to host servers only `tools/net/run.py` starts.
            env["LAZYOS_NETD"] = "1"
            env.setdefault("LAZYOS_NETD_ARGS", "demo=0")
        if args.desktop:
            env["LAZYOS_DESKTOP"] = "1"
            # One build for every missing app: the desktop's own, and the
            # network apps a `--net` desktop also ships.
            needed = DESKTOP_ELFS + (NET_APPS if args.net else [])
            if not all(app.is_file() for app in needed) and not build_xui_apps():
                return 1
        if args.mail:
            # After the other apps: one incremental build that adds Mail.
            if not build_mail():
                return 1
            env["LAZYOS_MAIL"] = "1"
        if args.usb_image:
            # The stick must ship `usbd` and boot `init` to start it: the
            # target PC may have no PS/2 port (the build refuses otherwise).
            env["LAZYOS_USB_IMAGE"] = "1"
            env["LAZYOS_USB"] = "1"
            if not args.desktop:
                env["LAZYOS_SERVICES"] = "1"
        if args.devices:
            if not build_xui_apps():
                return 1
            env["LAZYOS_XUI_AUTOSTART"] = with_devices(env.get("LAZYOS_XUI_AUTOSTART"))
        if args.timer:
            env["LAZYOS_TIMER"] = args.timer
        if args.no_shell:
            env["LAZYOS_SHELL"] = "0"
        elif args.desktop and env.get("LAZYOS_SHELL") != "0" and not build_xui_shell():
            return 1
        if args.desktop and not build_core_packages():
            return 1
        result = subprocess.run(cargo, cwd=ROOT, env=env)
        if result.returncode != 0:
            return result.returncode
        if args.build_only:
            return 0

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
        command += sound_args(args.sound, args.sound_card)
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
