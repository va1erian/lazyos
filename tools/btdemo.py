"""The fake Bluetooth dongle for QEMU boots (`tools/bt/run.py`,
`run_demo.py --bt`, the launcher's Bluetooth switch): the build variables,
`fakebt` as a background process, and the QEMU arguments that plug it in.

QEMU has no Bluetooth emulation of its own, so the dongle is a USB device
served over usbredir: `fakebt` listens on a local TCP port, QEMU's `usb-redir`
connects to it (`-chardev socket ... -device usb-redir`), and the guest sees a
class `E0/01/01` controller on its own `qemu-xhci` (docs/bluetooth-plan.md,
phase B2).
"""

from __future__ import annotations

import subprocess
import sys
import threading
from pathlib import Path
from typing import IO

ROOT = Path(__file__).resolve().parent.parent
EXE = ".exe" if sys.platform == "win32" else ""
FAKEBT = ROOT / "target" / "debug" / f"fakebt{EXE}"
#: The address the fake controller reports (Read_BD_ADDR), most significant
#: octet first; the judge checks the guest saw its last two octets.
BD_ADDR = "00:1A:7D:DA:71:13"
#: How long the harness waits for `fakebt` to say which port it listens on.
LISTEN_TIMEOUT = 30.0


def build_env() -> dict[str, str]:
    """The image's switches: `usbd` binds the controller (it needs `usbd`,
    and a services session starts it) and `init` runs `btd`."""
    return {"LAZYOS_BT": "1", "LAZYOS_USB": "1"}


def build_fakebt() -> Path:
    """`cargo build -p fakebt` (a host tool: std only, no OS build)."""
    result = subprocess.run(["cargo", "build", "-p", "fakebt"], cwd=ROOT)
    if result.returncode != 0 or not FAKEBT.is_file():
        sys.exit("cargo build -p fakebt failed")
    return FAKEBT


def qemu_args(port: int) -> list[str]:
    """The dongle's own xHCI controller and the redirection to `fakebt`."""
    return ["-device", "qemu-xhci,id=btxhci",
            "-chardev", f"socket,id=btredir,host=127.0.0.1,port={port}",
            "-device", "usb-redir,chardev=btredir,bus=btxhci.0"]


def _pump(stream: IO[str], log: Path | None) -> None:
    """Copy `fakebt`'s output to `log` until it exits (or drop it)."""
    with (open(log, "a", encoding="utf-8") if log else open(sys.devnull, "w")) as out:
        for line in stream:
            out.write(line)
            out.flush()


def start(log: Path | None = None, *, bd_addr: str = BD_ADDR, unplug_after_ms: int | None = None,
          connect_after_ms: int = 0, build: bool = True) -> tuple[subprocess.Popen, list[str]]:
    """Start `fakebt` and return it with the QEMU arguments of the dongle."""
    exe = build_fakebt() if build else FAKEBT
    command = [str(exe), "--listen", "127.0.0.1:0", "--bd-addr", bd_addr,
               "--connect-after-ms", str(connect_after_ms)]
    if unplug_after_ms is not None:
        command += ["--unplug-after-ms", str(unplug_after_ms)]
    if log:
        log.parent.mkdir(parents=True, exist_ok=True)
        log.write_text("")
    process = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    assert process.stdout is not None
    line = process.stdout.readline()
    if not line.startswith("fakebt: listening port="):
        process.kill()
        sys.exit(f"fakebt did not start: {line.strip()!r}")
    port = int(line.rsplit("=", 1)[1])
    threading.Thread(target=_pump, args=(process.stdout, log), daemon=True).start()
    return process, qemu_args(port)


def stop(process: subprocess.Popen | None) -> None:
    if process is None:
        return
    process.terminate()
    try:
        process.wait(timeout=5)
    except subprocess.TimeoutExpired:
        process.kill()


def describe(log: Path | None) -> str:
    where = f"; its log: {log}" if log else ""
    return ("Bluetooth: a fake USB dongle (tools/bt/fakebt, 0a12:0001, address "
            f"{BD_ADDR}) on its own xHCI controller; btd brings it up{where}")
