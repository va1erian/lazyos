"""Boot a `tools/lazyvm/run.py` image headless and follow the `lazyvm` fixture's progress.

QEMU writes the serial log to a file; `boot` reads what is new every `POLL` seconds and
hands it to a `Watch`, which decides when the run is over: the fixture printed
`LAZYVM:DONE` or `LAZYVM:FAIL`, the boot never reached it, or it stopped making progress.
A command's output streams as it is read, so the wait is not a fixed allowance: it lasts
while marker lines keep arriving, and a running command gets its whole timeout plus the
fixture's grace periods before silence counts against it.
"""

from __future__ import annotations

import subprocess
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "screenshot"))
from qemu_qmp import accel_args, build_qemu_command, find_qemu, free_port  # noqa: E402

#: Seconds the boot may take to reach the fixture.
BOOT_SECONDS = 300
#: Seconds without a marker line after which a run that is not inside a command's
#: timeout has stalled.
IDLE_SECONDS = 60
#: The fixture's grace after a program exits or is killed, twice (`GRACE` in lazyvm.rs).
GUEST_GRACE = 10
POLL = 0.2


class Watch:
    """The fixture's progress through the serial log, fed as it grows."""

    def __init__(self, timeout: int, now: float, boot: float = BOOT_SECONDS,
                 idle: float = IDLE_SECONDS):
        self.timeout, self.boot, self.idle = timeout, boot, idle
        self.start = self.last = now
        self.started = False
        self.command_start: float | None = None
        self.partial = ""
        self.verdict: str | None = None

    def feed(self, text: str, now: float) -> None:
        """Take the next piece of the log; only whole lines count."""
        lines = (self.partial + text).split("\n")
        self.partial = lines.pop()
        for line in lines:
            at = line.find("LAZYVM:")
            if at >= 0:
                self.marker(line[at + len("LAZYVM:"):].rstrip("\r"), now)

    def marker(self, rest: str, now: float) -> None:
        self.last = now
        kind = rest.split(":", 1)[0]
        if kind == "INIT":
            self.started = True
        elif kind == "CMD":
            self.command_start = now
        elif kind == "END":
            self.command_start = None
        elif kind == "DONE":
            self.verdict = "done"
        elif kind == "FAIL" and self.verdict is None:
            self.verdict = f"the fixture failed: {rest[len('FAIL:'):]}"

    def stalled(self, now: float) -> str | None:
        """Why the run should stop now, if it should (`None` keeps waiting)."""
        if not self.started:
            if now - self.start > self.boot:
                return f"the boot did not reach the fixture in {self.boot:.0f} s"
            return None
        deadline = self.last + self.idle
        if self.command_start is not None:
            deadline = max(deadline, self.command_start + self.timeout + GUEST_GRACE + self.idle)
        if now > deadline:
            return f"no progress from the fixture for {now - self.last:.0f} s"
        return None


def boot(image: Path, timeout: int, out_dir: Path, accel: str, memory: str,
         qemu: str | None) -> tuple[str, str | None]:
    """Run QEMU until the watch is satisfied; `(serial log, failure or None)`."""
    out_dir.mkdir(parents=True, exist_ok=True)
    log = out_dir / "serial.log"
    log.unlink(missing_ok=True)
    binary = find_qemu(qemu)
    command = build_qemu_command(binary, str(image), free_port(), log, memory,
                                 accel_args(accel, binary))
    stderr = out_dir / "qemu.stderr"
    with stderr.open("wb") as sink:
        proc = subprocess.Popen(command, stdout=subprocess.DEVNULL, stderr=sink)
    watch = Watch(timeout, time.monotonic())
    failure = None
    offset = 0
    try:
        while watch.verdict is None:
            time.sleep(POLL)
            now = time.monotonic()
            if log.is_file():
                with log.open("rb") as handle:
                    handle.seek(offset)
                    data = handle.read()
                offset += len(data)
                watch.feed(data.decode("utf-8", errors="replace"), now)
            if watch.verdict is None and proc.poll() is not None:
                failure = f"QEMU exited with status {proc.returncode}"
                break
            failure = watch.stalled(now) if watch.verdict is None else None
            if failure:
                break
    finally:
        proc.kill()
        proc.wait()
    errors = stderr.read_text(encoding="utf-8", errors="replace").strip()
    if failure and errors:
        failure += f"\n{errors[-1500:]}"
    if watch.verdict and watch.verdict != "done":
        failure = watch.verdict
    text = log.read_text(encoding="utf-8", errors="replace") if log.is_file() else ""
    return text, failure
