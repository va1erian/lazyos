#!/usr/bin/env python3
"""Install the stick beside "Linux" on a disk image and boot it alone
(docs/grub-install-plan.md I3).

1. **Build** the desktop stick image (``run_demo.py --desktop --usb-image
   --autologin user --build-only`` with the Terminal autostarted) and the
   host ``lazyos-install`` with its ``testdisk`` helper.
2. **Disk**: a Linux-shaped GPT disk (``testdisk disk``: a FAT32 ESP, a
   Linux root stand-in, ``lazyos-system``, ``lazyos-home``) and a loader on
   its ESP, one of:

   * ``--via grub``: real GRUB, ``grub-mkstandalone`` as ``\\EFI\\BOOT``'s
     removable-media loader, a Debian-shaped ``grub.cfg`` whose default is a
     "Linux" stand-in, ``grubenv`` with ``next_entry=lazyos`` (what
     ``grub-reboot LazyOS`` writes) and upstream's ``custom.cfg`` source line.
     Needs ``grub-mkstandalone`` and the ``x86_64-efi`` modules (Linux, CI);
   * ``--via shell`` (default where GRUB is missing): OVMF's shell runs
     ``startup.nsh``, which starts ``\\EFI\\lazyos\\BOOTX64.EFI`` the way
     GRUB's ``chainloader`` does (its device the ESP, its file path in
     ``\\EFI\\lazyos``). Everything but GRUB itself, on any host.
3. **Install** with ``lazyos-install --image`` (test mode: the same volume,
   ESP and checker code as on a real disk; the entry goes into
   ``EFI/BOOT/custom.cfg``). The "Linux" files must stay byte-identical.
4. **Boot 1**: the disk alone on QEMU's AHCI controller under OVMF, no
   stick: the desktop, root ``ahci0p3`` by UUID, ``/home`` ``ahci0p4``,
   ``/boot`` the config ramdisk, then ``shutdown`` from the Terminal.
5. **Host**: the checker on both partitions, and what LazyOS wrote
   (``/apps``, ``/logs``).
6. GRUB only: a boot with no ``next_entry`` is the "Linux" default again.
7. **Boot 2**: LazyOS again over the first boot's state.

    python tools/install/run.py                    # build, then everything
    python tools/install/run.py --no-build --via shell
    python tools/install/test_judge.py             # the judge fails when it should

Writes ``shots/install/<via>/`` (logs, screenshots, ``report.json``); exit
status is non-zero on any failure.
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(ROOT / "tools" / "screenshot"))

import judge  # noqa: E402
import pngstats  # noqa: E402
import qemu_qmp  # noqa: E402

STICK = ROOT / "target" / "lazyos-usb.img"
EXE = ".exe" if os.name == "nt" else ""
INSTALLER = ROOT / "target" / "debug" / f"lazyos-install{EXE}"
TESTDISK = ROOT / "target" / "debug" / "examples" / f"testdisk{EXE}"
GRUB_FILES = ("EFI/BOOT/BOOTX64.EFI", "EFI/BOOT/grub.cfg")
STUB_CFG = """search --no-floppy --set=root --file /EFI/BOOT/grub.cfg
set prefix=($root)/EFI/BOOT
configfile $prefix/grub.cfg
"""
# Shaped like Debian's generated grub.cfg: 00_header's next_entry handling,
# a default "Linux" entry, and 41_custom's custom.cfg source line.
GRUB_CFG = """serial --unit=0 --speed=115200
terminal_output console serial
set timeout=1
load_env
if [ "${next_entry}" ] ; then
   set default="${next_entry}"
   set next_entry=
   save_env next_entry
else
   set default=0
fi
menuentry "Linux (stand-in)" --id linux {
	echo "LINUX:STANDIN"
	sleep 2
	halt
}
if [ -f ${prefix}/custom.cfg ]; then
  source ${prefix}/custom.cfg
fi
"""
STARTUP_NSH = "@echo -off\r\nfs0:\r\n\\EFI\\lazyos\\BOOTX64.EFI\r\n"


def say(text: str) -> None:
    print(f"install-harness: {text}", flush=True)


def grubenv(next_entry: str | None) -> bytes:
    """A 1 KiB GRUB environment block, as `grub-editenv` writes it."""
    text = "# GRUB Environment Block\n" + (f"next_entry={next_entry}\n" if next_entry else "")
    return (text + "#" * (1024 - len(text))).encode()


def run(command: list, log: Path | None = None, check: bool = True, env: dict | None = None) -> str:
    result = subprocess.run([str(c) for c in command], cwd=ROOT, capture_output=True, text=True, env=env)
    output = result.stdout + result.stderr
    if log:
        log.write_text(output, encoding="utf-8")
    if check and result.returncode != 0:
        raise SystemExit(f"{' '.join(map(str, command))} failed ({result.returncode}):\n{output[-3000:]}")
    return output


def build() -> None:
    env = dict(os.environ, LAZYOS_XUI_AUTOSTART="term")
    say("building the desktop stick image (run_demo.py --desktop --usb-image)")
    run([sys.executable, ROOT / "tools/run_demo.py", "--desktop", "--usb-image",
         "--autologin", "user", "--build-only"], env=env)


def ovmf(explicit_code: str | None, explicit_vars: str | None, qemu: str) -> tuple[Path, Path]:
    """OVMF's code and variable store: given, a distro's, or QEMU's own."""
    share = Path(qemu).resolve().parent / "share"
    codes = [Path("/usr/share/OVMF/OVMF_CODE_4M.fd"), Path("/usr/share/OVMF/OVMF_CODE.fd"),
             Path("/usr/share/edk2/ovmf/OVMF_CODE.fd"), share / "edk2-x86_64-code.fd"]
    stores = [Path("/usr/share/OVMF/OVMF_VARS_4M.fd"), Path("/usr/share/OVMF/OVMF_VARS.fd"),
              Path("/usr/share/edk2/ovmf/OVMF_VARS.fd"), share / "edk2-i386-vars.fd"]
    code = Path(explicit_code) if explicit_code else next((p for p in codes if p.is_file()), None)
    store = Path(explicit_vars) if explicit_vars else next((p for p in stores if p.is_file()), None)
    if not code or not store:
        raise SystemExit("no OVMF found; install ovmf or pass --ovmf-code/--ovmf-vars")
    return code, store


def make_disk(args, out: Path) -> Path:
    disk = out / "disk.img"
    stick = out / "stick"
    shutil.rmtree(stick, ignore_errors=True)
    say(run([TESTDISK, "unpack-stick", STICK, stick]).strip())
    say(run([TESTDISK, "disk", disk, "--system", args.system_mib, "--home", args.home_mib]).strip())
    if args.via == "shell":
        (out / "startup.nsh").write_bytes(STARTUP_NSH.encode())
        run([TESTDISK, "esp-put", disk, out / "startup.nsh", "startup.nsh"])
        return disk
    (out / "stub.cfg").write_text(STUB_CFG)
    (out / "grub.cfg").write_text(GRUB_CFG)
    run(["grub-mkstandalone", "-O", "x86_64-efi", "-o", out / "BOOTX64.EFI",
         "--modules=part_gpt fat chain search configfile normal loadenv echo halt serial sleep test",
         f"boot/grub/grub.cfg={out / 'stub.cfg'}"])
    run([TESTDISK, "esp-put", disk, out / "BOOTX64.EFI", "EFI/BOOT/BOOTX64.EFI"])
    run([TESTDISK, "esp-put", disk, out / "grub.cfg", "EFI/BOOT/grub.cfg"])
    return disk


def esp_files(disk: Path, out: Path, names: tuple[str, ...]) -> dict[str, bytes]:
    files = {}
    for name in names:
        target = out / ("esp-" + name.replace("/", "_"))
        run([TESTDISK, "esp-get", disk, name, target])
        files[name] = target.read_bytes()
    return files


def set_next_entry(disk: Path, out: Path, entry: str | None) -> None:
    (out / "grubenv").write_bytes(grubenv(entry))
    run([TESTDISK, "esp-put", disk, out / "grubenv", "EFI/BOOT/grubenv"])


def qemu_disk_args(disk: Path, firmware: tuple[Path, Path], out: Path, name: str) -> list[str]:
    code, store = firmware
    vars_copy = out / f"{name}-vars.fd"
    shutil.copyfile(store, vars_copy)
    return ["-nic", "none",
            "-drive", f"if=pflash,format=raw,unit=0,readonly=on,file={code.as_posix()}",
            "-drive", f"if=pflash,format=raw,unit=1,file={vars_copy.as_posix()}",
            "-device", "qemu-xhci,id=xhci", "-device", "usb-kbd,bus=xhci.0",
            "-device", "usb-mouse,bus=xhci.0",
            "-device", "ahci,id=ahcib",
            "-drive", f"if=none,id=disk,format=raw,file={disk.resolve().as_posix()}",
            "-device", "ide-hd,drive=disk,bus=ahcib.0,bootindex=0"]


def session(args, name: str, steps: list[dict], extra: list[str], out: Path) -> str:
    run_dir = out / name
    shutil.rmtree(run_dir, ignore_errors=True)
    run_dir.mkdir(parents=True)
    script = out / f"{name}.json"
    script.write_text(json.dumps(steps, indent=1), encoding="utf-8")
    command = [sys.executable, ROOT / "tools/screenshot/qemu_session.py",
               "--accel", args.accel, "--memory", args.memory,
               "--timeout", str(args.timeout), "--wait-timeout", str(args.timeout),
               "--out", run_dir, "--script", script, "--fail-on", r"LazyOS PANIC|panicked at",
               ] + [f"--extra-arg={arg}" for arg in extra]
    started = time.time()
    output = run(command, log=out / f"{name}.session.log", check=False)
    say(f"{name}: session finished in {time.time() - started:.0f} s")
    if "Traceback" in output:
        say(output[-1500:])
    log = run_dir / "serial.log"
    return log.read_text(encoding="utf-8", errors="replace") if log.exists() else ""


def screen_stats(out: Path, name: str) -> dict:
    shots = sorted((out / name).glob("*desktop*.png"))
    if not shots:
        return {"error": "no screenshot"}
    return pngstats.analyse_file(str(shots[-1]), None, None, None, None, None)[1]


def desktop_steps(power_off: bool) -> list[dict]:
    steps = [{"wait_for": "SHELL:DESKTOP:PASS", "timeout": 900},
             {"wait_for": "PKGD:PROVISION:DONE", "timeout": 600},
             {"wait_for": "TERM:UP:PASS", "timeout": 120},
             {"at": 3.0, "shot": "desktop"}]
    if not power_off:
        return steps
    # The Terminal (autostarted) takes the keyboard; then `shutdown` asks init.
    steps += [{"at": 1.0, "mouse_move": [-300, -300]}, {"at": 1.2, "mouse_move": [-300, -300]},
              {"at": 1.4, "mouse_move": [-300, -300]}, {"at": 1.8, "mouse_move": [400, 58]},
              {"at": 2.2, "mouse_click": "left"},
              {"at": 3.0, "type": "echo ready"},
              {"at": 3.5, "key": "enter", "until": "TERM:OUT:ready", "timeout": 30, "retries": 2},
              {"at": 1.0, "type": "shutdown"},
              {"at": 1.5, "key": "enter", "until": "INIT:SHUTDOWN:BEGIN", "timeout": 30, "retries": 2},
              {"wait_for": judge.SYNCED, "timeout": 120}]
    return steps


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--via", choices=["grub", "shell"], default=None,
                        help="the loader before LazyOS (default: grub when grub-mkstandalone exists)")
    parser.add_argument("--no-build", action="store_true")
    parser.add_argument("--out", type=Path, default=None)
    parser.add_argument("--system-mib", default="2048")
    parser.add_argument("--home-mib", default="256")
    parser.add_argument("--accel", default="auto")
    parser.add_argument("--memory", default="2G")
    parser.add_argument("--timeout", type=float, default=1200.0)
    parser.add_argument("--qemu", default=None)
    parser.add_argument("--ovmf-code", default=None)
    parser.add_argument("--ovmf-vars", default=None)
    args = parser.parse_args()
    args.via = args.via or ("grub" if shutil.which("grub-mkstandalone") else "shell")
    out = args.out or ROOT / "shots" / "install" / args.via
    out.mkdir(parents=True, exist_ok=True)
    if not args.no_build:
        build()
    run(["cargo", "build", "-q", "-p", "lazyos-install", "--bins", "--examples"])
    if not STICK.is_file():
        raise SystemExit(f"{STICK} missing; build it (drop --no-build)")
    qemu = qemu_qmp.find_qemu(args.qemu)
    firmware = ovmf(args.ovmf_code, args.ovmf_vars, qemu)
    report: dict = {"via": args.via, "failures": {}}

    disk = make_disk(args, out)
    before = esp_files(disk, out, GRUB_FILES) if args.via == "grub" else {}
    install_log = run([INSTALLER, "--image", disk, "--stick", out / "stick", "--yes",
                       "--grub-dir", "EFI/BOOT", "--check-limit", "4096"],
                      log=out / "install.log", check=False)
    after = esp_files(disk, out, GRUB_FILES) if args.via == "grub" else {}
    uuids = judge.install_uuids(install_log)
    report["failures"]["install"] = judge.judge_install(install_log, before, after)
    report["uuids"] = uuids

    stages = [("boot1", True), ("boot2", False)]
    for name, power_off in stages:
        if any(report["failures"].values()):
            break
        if args.via == "grub":
            set_next_entry(disk, out, "lazyos")
        log = session(args, name, desktop_steps(power_off),
                      qemu_disk_args(disk, firmware, out, name), out)
        report["failures"][name] = judge.judge_boot(log, screen_stats(out, name), uuids, power_off)
        if name == "boot1" and not report["failures"][name]:
            check = run([TESTDISK, "check", disk], log=out / "host.log", check=False)
            report["host"] = check.strip().splitlines()
            report["failures"]["host"] = judge.judge_host(check)
            if args.via == "grub":
                log = session(args, "linux", [{"wait_for": judge.LINUX_STANDIN, "timeout": 120},
                                              {"wait": 5}],
                              qemu_disk_args(disk, firmware, out, "linux"), out)
                report["failures"]["linux"] = judge.judge_linux(log)

    failures = [f"{stage}: {f}" for stage, items in report["failures"].items() for f in items]
    (out / "report.json").write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report, indent=2))
    print(("FAIL: " + "; ".join(failures)) if failures else "PASS", flush=True)
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
