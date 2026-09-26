#!/usr/bin/env python3
"""Boot a disk image (or bare firmware) in headless QEMU and capture PNG
screenshots through the QEMU Machine Protocol (QMP).

This is agent/CI tooling: it lets a headless runner (or an AI agent) obtain
pixels from the emulated display so graphical output can be verified without a
human looking at a window.

Design constraints
------------------
* Pure Python standard library only (no pip installs) so it runs anywhere.
* Cross-platform (Windows, Linux, macOS). QMP is exposed over TCP, not a UNIX
  socket, because Windows UNIX-socket support is unreliable.
* Uses `screendump ...,format=png` (QEMU >= 7.1). Falls back to PPM on older
  QEMU, recording the real path in the summary.

Usage
-----
    python tools/screenshot/qemu_shot.py --image target/lazyos.img \
        --out shots --at 2,5,10 --extra-arg=-vga --extra-arg=std

    # No --image => boots the firmware only (SeaBIOS "no bootable device"),
    # useful to smoke-test the capture pipeline itself.
    python tools/screenshot/qemu_shot.py --out shots --at 2,5

Outputs
-------
    <out>/shot_<t>s.png   one screenshot per requested time
    <out>/serial.log      serial console output (COM1)
    <out>/summary.json    machine-readable manifest (paths, exit code, qemu)
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import socket
import subprocess
import sys
import time
from pathlib import Path

_WINDOWS_QEMU = (
    r"C:\Program Files\qemu\qemu-system-x86_64.exe",
    r"C:\Program Files (x86)\qemu\qemu-system-x86_64.exe",
)


def find_qemu(explicit: str | None = None) -> str:
    """Locate qemu-system-x86_64, honouring --qemu, PATH, then common dirs."""
    if explicit:
        if not os.path.isfile(explicit):
            sys.exit(f"--qemu path does not exist: {explicit}")
        return explicit
    found = shutil.which("qemu-system-x86_64")
    if found:
        return found
    for candidate in _WINDOWS_QEMU:
        if os.path.isfile(candidate):
            return candidate
    sys.exit(
        "qemu-system-x86_64 not found. Install QEMU or pass --qemu PATH. "
        "(Windows: add 'C:\\Program Files\\qemu' to PATH)."
    )


def free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return int(s.getsockname()[1])


class Qmp:
    """Minimal QMP client: handshake, execute commands, ignore async events."""

    def __init__(self, host: str, port: int, timeout: float):
        deadline = time.time() + timeout
        last_err: Exception | None = None
        self.sock: socket.socket | None = None
        while time.time() < deadline:
            try:
                self.sock = socket.create_connection((host, port), timeout=2)
                break
            except OSError as exc:  # QEMU not listening yet
                last_err = exc
                time.sleep(0.2)
        if self.sock is None:
            raise RuntimeError(f"could not connect to QMP at {host}:{port}: {last_err}")

        self.sock.settimeout(timeout)
        self._file = self.sock.makefile("rwb", buffering=0)
        greeting = self._read_message()
        if "QMP" not in greeting:
            raise RuntimeError(f"unexpected QMP greeting: {greeting}")
        self._id = 0
        self.execute("qmp_capabilities")

    def _read_message(self) -> dict:
        line = self._file.readline()
        if not line:
            raise RuntimeError("QMP connection closed unexpectedly")
        return json.loads(line.decode("utf-8"))

    def execute(self, command: str, **arguments) -> dict:
        self._id += 1
        request: dict = {"execute": command, "id": self._id}
        if arguments:
            request["arguments"] = arguments
        self._file.write((json.dumps(request) + "\n").encode("utf-8"))
        while True:
            message = self._read_message()
            if "event" in message:
                continue
            if message.get("id") != self._id:
                continue
            if "error" in message:
                raise RuntimeError(f"QMP {command} failed: {message['error']}")
            return message.get("return", {})

    def screenshot(self, dest: Path) -> Path:
        """Capture the primary display, preferring PNG; fall back to PPM."""
        png = dest.with_suffix(".png")
        try:
            self.execute("screendump", filename=png.as_posix(), format="png")
            return png
        except RuntimeError as exc:
            if "format" not in str(exc).lower():
                raise
            # QEMU < 7.1: no `format` parameter, PPM only.
            ppm = dest.with_suffix(".ppm")
            self.execute("screendump", filename=ppm.as_posix())
            return ppm

    def close(self) -> None:
        try:
            self._file.close()
        except Exception:
            pass
        if self.sock is not None:
            try:
                self.sock.close()
            except Exception:
                pass


def parse_times(raw: str) -> list[float]:
    times = []
    for piece in raw.split(","):
        piece = piece.strip()
        if not piece:
            continue
        value = float(piece)
        if value < 0:
            sys.exit(f"--at values must be >= 0 (got {value})")
        times.append(value)
    if not times:
        sys.exit("--at must contain at least one timestamp")
    return times


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--image", help="raw disk image to boot (omit to boot firmware only)")
    parser.add_argument("--out", default="shots", help="output directory (default: shots)")
    parser.add_argument("--at", default="3,6,10", help="comma-separated capture times in seconds")
    parser.add_argument("--qemu", help="path to qemu-system-x86_64")
    parser.add_argument("--timeout", type=float, default=180.0, help="QMP/overall timeout in seconds")
    parser.add_argument("--memory", default="256M", help="guest RAM (default: 256M)")
    parser.add_argument("--extra-arg", action="append", default=[], metavar="ARG",
                        help="extra QEMU argument; repeat for multiple")
    args = parser.parse_args()

    qemu = find_qemu(args.qemu)
    out_dir = Path(args.out).resolve()
    out_dir.mkdir(parents=True, exist_ok=True)
    times = parse_times(args.at)
    port = free_port()
    serial_log = out_dir / "serial.log"

    command = [
        qemu,
        "-display", "none",
        "-no-reboot",
        "-qmp", f"tcp:127.0.0.1:{port},server=on,wait=off",
        "-serial", f"file:{serial_log.as_posix()}",
        "-m", args.memory,
    ]
    image: str | None = None
    if args.image:
        image_path = Path(args.image).resolve()
        if not image_path.is_file():
            sys.exit(f"--image not found: {image_path}")
        image = str(image_path)
        command += ["-drive", f"format=raw,file={image_path.as_posix()}"]
    command += args.extra_arg

    print(f"launching: {' '.join(command)}", flush=True)
    proc = subprocess.Popen(command, stdout=subprocess.DEVNULL, stderr=subprocess.STDOUT)

    screenshots: list[str] = []
    qmp: Qmp | None = None
    try:
        qmp = Qmp("127.0.0.1", port, args.timeout)
        started = time.time()
        for moment in times:
            remaining = moment - (time.time() - started)
            if remaining > 0:
                time.sleep(remaining)
            shot = qmp.screenshot(out_dir / f"shot_{moment:g}s")
            screenshots.append(shot.name)
            print(f"captured {shot}", flush=True)
        try:
            qmp.execute("quit")
        except Exception:
            pass
    finally:
        if qmp is not None:
            qmp.close()
        try:
            proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            proc.terminate()
            try:
                proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                proc.kill()

    summary = {
        "qemu": qemu,
        "image": image,
        "screenshots": screenshots,
        "serial_log": serial_log.name if serial_log.exists() else None,
        "exit_code": proc.returncode,
    }
    (out_dir / "summary.json").write_text(json.dumps(summary, indent=2), encoding="utf-8")
    print(json.dumps(summary, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
