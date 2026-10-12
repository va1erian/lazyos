#!/usr/bin/env python3
"""Launch QEMU + OVMF with no disk and a network card, for the Q0 spike.

    python tools/netboot/spike/qemu_spike.py [--mode pxe|rom] [--memory 2G]
        [--serial FILE] [--timeout S] [--accel auto|whpx|none] [--headless]

It judges nothing; it is the launcher a person drives (docs/netboot-plan.md,
phase Q0; `SPIKE.md` has the checklist). The exact command line is printed
first. The machine has OVMF in two pflash units (a fresh private copy of the
variable store under `target/netboot/`, so no run inherits another's boot
entries), **no disk**, a USB keyboard and mouse on a `qemu-xhci`, serial in a
file, and a QMP socket (`tools/screenshot/qemu_qmp.py` can screenshot it).
The QMP monitor listens on a free loopback TCP port (picked with
`bind(("127.0.0.1", 0))`, like every harness there) and is unauthenticated:
anything on this machine that finds the port can drive the guest, so the
launcher is for a single-user dev machine.

* `--mode pxe` (default): a `virtio-net-pci` with its option ROM off, so OVMF's
  own network stack PXE-boots from slirp's TFTP (`tftp=DIR,bootfile=ipxe.efi`)
  into the official `ipxe.efi`. This is the path the real machine's firmware
  takes.
* `--mode rom`: the NIC's own option ROM (`efi-virtio.rom`, itself an iPXE
  build) and no boot file; the TFTP directory is still served, for
  `autoexec.ipxe`. An alternative if the first path misbehaves.

The guest sees this machine's loopback as 10.0.2.2 (slirp), so `serve.py`
bound to 127.0.0.1:8420 is `http://10.0.2.2:8420` in the guest.
"""

from __future__ import annotations

import argparse
import shlex
import shutil
import subprocess
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))  # `python -I` drops the script dir

import common  # noqa: E402
import qemu_qmp  # noqa: E402

NETDEV_ID = "n0"
#: Where OVMF's code and variable store come from, after QEMU's own `share/`.
_SYSTEM_CODE = ["/usr/share/OVMF/OVMF_CODE_4M.fd", "/usr/share/OVMF/OVMF_CODE.fd",
                "/usr/share/edk2/ovmf/OVMF_CODE.fd", "/usr/share/qemu/OVMF_CODE.fd"]
_SYSTEM_VARS = ["/usr/share/OVMF/OVMF_VARS_4M.fd", "/usr/share/OVMF/OVMF_VARS.fd",
                "/usr/share/edk2/ovmf/OVMF_VARS.fd", "/usr/share/qemu/OVMF_VARS.fd"]
#: QEMU ships no x86_64 variable store on Windows; the i386 one is the same layout.
_SHARE_CODE = ["edk2-x86_64-code.fd"]
_SHARE_VARS = ["edk2-x86_64-vars.fd", "edk2-i386-vars.fd"]


def find_firmware(qemu: str, explicit: str | None, names: list[str], system: list[str], what: str) -> Path:
    """`explicit`, else the first of QEMU's `share/<name>`, else the system paths."""
    if explicit:
        path = Path(explicit)
        if not path.is_file():
            raise SystemExit(f"{what}: {path} does not exist")
        return path
    share = Path(qemu).resolve().parent / "share"
    candidates = [share / n for n in names] + [Path(p) for p in system]
    for path in candidates:
        if path.is_file():
            return path
    raise SystemExit(f"no {what} found (tried {', '.join(map(str, candidates))}); pass --ovmf-code/--ovmf-vars")


def qemu_path(path: Path) -> str:
    """A path as a QEMU option value: forward slashes, commas doubled."""
    return path.resolve().as_posix().replace(",", ",,")


def netdev_args(mode: str, tftp_dir: Path, bootfile: str) -> list[str]:
    """The user-mode network and its card for `mode`."""
    if mode not in ("pxe", "rom"):
        raise ValueError(f"mode must be pxe or rom, not {mode!r}")
    netdev = f"user,id={NETDEV_ID},tftp={qemu_path(tftp_dir)}"
    device = f"virtio-net-pci,netdev={NETDEV_ID},bootindex=0"
    if mode == "pxe":
        netdev += f",bootfile={bootfile}"
        device += ",romfile="  # OVMF's own driver and PXE stack, not the ROM's
    return ["-netdev", netdev, "-device", device]


def build_command(qemu: str, *, mode: str, memory: str, serial: Path, qmp_port: int,
                  code: Path, vars_copy: Path, tftp_dir: Path, bootfile: str = "ipxe.efi",
                  machine: str = "q35", accel_args: list[str] | None = None,
                  headless: bool = False, no_reboot: bool = False,
                  extra: list[str] | None = None) -> list[str]:
    """The whole QEMU command line (pure; no disk is attached)."""
    command = [qemu, "-machine", machine, "-m", memory,
               "-drive", f"if=pflash,format=raw,unit=0,readonly=on,file={qemu_path(code)}",
               "-drive", f"if=pflash,format=raw,unit=1,file={qemu_path(vars_copy)}"]
    command += accel_args or []
    command += netdev_args(mode, tftp_dir, bootfile)
    command += ["-device", "qemu-xhci,id=xhci", "-device", "usb-kbd,bus=xhci.0",
                "-device", "usb-mouse,bus=xhci.0",
                "-serial", f"file:{qemu_path(serial)}",
                "-qmp", f"tcp:127.0.0.1:{qmp_port},server=on,wait=off"]
    if headless:
        command += ["-display", "none"]
    if no_reboot:
        command += ["-no-reboot"]
    return command + (extra or [])


def parse_times(text: str) -> list[float]:
    return sorted(float(t) for t in text.split(",") if t.strip())


def run(command: list[str], timeout: float, shot_at: list[float], shot_dir: Path, qmp_port: int) -> int:
    """Run QEMU until it exits, `timeout` (0: none) or ^C, taking screenshots on the way."""
    started = time.monotonic()
    process = subprocess.Popen(command)
    pending = list(shot_at)
    try:
        while process.poll() is None:
            elapsed = time.monotonic() - started
            if timeout and elapsed > timeout:
                print(f"timeout after {timeout:.0f} s: stopping QEMU", flush=True)
                break
            if pending and elapsed >= pending[0]:
                at = pending.pop(0)
                shot_dir.mkdir(parents=True, exist_ok=True)
                try:
                    qmp = qemu_qmp.Qmp("127.0.0.1", qmp_port, timeout=10)
                    print("screenshot:", qmp.screenshot(shot_dir / f"shot_{int(at):04d}s"), flush=True)
                    qmp.close()
                except (RuntimeError, OSError) as exc:
                    print(f"screenshot at {at:.0f} s failed: {exc}", flush=True)
            time.sleep(0.5)
    except KeyboardInterrupt:
        print("interrupted: stopping QEMU", flush=True)
    finally:
        if process.poll() is None:
            process.kill()
        process.wait()
    return process.returncode or 0


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0],
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--mode", choices=["pxe", "rom"], default="pxe")
    ap.add_argument("--memory", default="2G")
    ap.add_argument("--serial", default=str(common.SPIKE_DIR / "serial.log"))
    ap.add_argument("--timeout", type=float, default=0, help="stop QEMU after this many seconds (0: never)")
    ap.add_argument("--accel", default="auto", help="auto, whpx, kvm or none (TCG)")
    ap.add_argument("--machine", default="q35", help="QEMU machine type (default %(default)s; pc is the other)")
    ap.add_argument("--tftp-dir", default=str(common.DEFAULT_IPXE),
                    help="the directory fetch_ipxe.py filled (default %(default)s)")
    ap.add_argument("--bootfile", default="ipxe.efi")
    ap.add_argument("--headless", action="store_true", help="no window (-display none); use --shot-at")
    ap.add_argument("--no-reboot", action="store_true")
    ap.add_argument("--qmp-port", type=int, default=0, help="QMP TCP port on the loopback (default: a free one)")
    ap.add_argument("--shot-at", default="", help="seconds after start to screenshot, e.g. 20,60")
    ap.add_argument("--shot-dir", default=str(common.SPIKE_DIR / "shots"))
    ap.add_argument("--keep-vars", action="store_true", help="reuse the previous OVMF variable store")
    ap.add_argument("--qemu")
    ap.add_argument("--ovmf-code")
    ap.add_argument("--ovmf-vars")
    ap.add_argument("--dry-run", action="store_true", help="print the command and stop")
    ap.add_argument("--extra-arg", action="append", default=[], help="extra QEMU argument (repeatable)")
    args = ap.parse_args(argv)

    qemu = qemu_qmp.find_qemu(args.qemu)
    tftp_dir = Path(args.tftp_dir)
    if args.mode == "pxe" and not (tftp_dir / args.bootfile).is_file():
        print(f"warning: {tftp_dir / args.bootfile} does not exist (run fetch_ipxe.py)", file=sys.stderr)
    code = find_firmware(qemu, args.ovmf_code, _SHARE_CODE, _SYSTEM_CODE, "OVMF code image")
    vars_source = find_firmware(qemu, args.ovmf_vars, _SHARE_VARS, _SYSTEM_VARS, "OVMF variable store")
    common.SPIKE_DIR.mkdir(parents=True, exist_ok=True)
    vars_copy = common.SPIKE_DIR / "OVMF_VARS.fd"
    if not args.keep_vars or not vars_copy.exists():
        if not args.dry_run:
            shutil.copyfile(vars_source, vars_copy)
    serial = Path(args.serial)
    serial.parent.mkdir(parents=True, exist_ok=True)
    port = args.qmp_port or qemu_qmp.free_port()
    command = build_command(
        qemu, mode=args.mode, memory=args.memory, serial=serial, qmp_port=port, code=code,
        vars_copy=vars_copy, tftp_dir=tftp_dir, bootfile=args.bootfile, machine=args.machine,
        accel_args=[] if args.dry_run else qemu_qmp.accel_args(args.accel, qemu),
        headless=args.headless, no_reboot=args.no_reboot, extra=args.extra_arg)
    print("qemu:", shlex.join(command) if sys.platform != "win32" else subprocess.list2cmdline(command),
          flush=True)
    print(f"serial log: {serial}   QMP: 127.0.0.1:{port}   guest sees this machine as 10.0.2.2", flush=True)
    if args.dry_run:
        return 0
    serial.write_text("")
    return run(command, args.timeout, parse_times(args.shot_at), Path(args.shot_dir), port)


if __name__ == "__main__":
    sys.exit(main())
