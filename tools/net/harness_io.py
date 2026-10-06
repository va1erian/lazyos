"""Following a network harness boot: the serial log and QEMU's exit.

Split out of `tools/net/run.py` (file-size budget, issue #497); `run.py` and
any variant harness share these.
"""

from __future__ import annotations

import subprocess
import time
from pathlib import Path

#: Serial lines worth echoing while the guest runs.
LOG_PREFIXES = ("NET", "DEV:CROSSCLAIM", "netdrv", "netd", "NICCTL", "NETDRV", "NETD", "NETCTL", "PING", "NC:",
                "NSLOOKUP", "Looking up", "FTP:", "< ", "> ", "NETFIX:", "ABI:netfix", "DEVD")


def wait_for_marker(serial_log: Path, proc: subprocess.Popen, timeout: float, done, fail_markers) -> str:
    """Poll the serial log until `done(text)` or a failure marker, or time runs out."""
    deadline = time.time() + timeout
    text = ""
    printed = 0
    while time.time() < deadline:
        if serial_log.is_file():
            text = serial_log.read_text(errors="replace")
            lines = text.splitlines()
            for line in lines[printed:]:
                if line.startswith(LOG_PREFIXES) or "PANIC" in line or "EXCEPTION" in line:
                    print(f"  {line}", flush=True)
            printed = len(lines)
            if done(text) or any(marker in text for marker in fail_markers):
                return text
        if proc.poll() is not None:
            break
        time.sleep(0.25)
    if serial_log.is_file():
        text = serial_log.read_text(errors="replace")
    return text


def stop_qemu(proc: subprocess.Popen, qmp) -> None:
    """Quit through QMP so the capture file is closed on the way out."""
    if qmp is not None:
        try:
            qmp.execute("quit")
        except Exception:
            pass
        qmp.close()
    try:
        proc.wait(timeout=15)
    except subprocess.TimeoutExpired:
        proc.terminate()
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()
