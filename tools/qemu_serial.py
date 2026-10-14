"""The guest's serial port for the interactive launchers (issue #857).

`run_demo.py` used to boot QEMU with `-serial mon:stdio`: the guest's COM1 and
the monitor on the console that started it. QEMU writes the serial bytes to
that console from the thread that executes the guest's `out` instruction, so a
console that stops reading (a Windows console paused by a selection, the GUI
launcher's reader busy, a full pipe) stalls the guest's vCPU inside a port
write, and the guest turns it into a frozen desktop. Now the serial port goes
to a file, which never blocks, and this launcher tails the file to its own
stdout from a thread: a console that stops reading stalls the tail, not the
guest. The file is also the whole serial log of the run (`target/serial.log`).

The monitor stays on the console, as before: a muxed stdio chardev, so
`Ctrl-A x` still quits QEMU, `Ctrl-A c` is the monitor prompt and `Ctrl-A h`
lists the keys. (Nothing the guest does reads its serial port, so the console's
keyboard never fed it.) `--serial-stdio` restores the old single muxed device,
now with the log file as a copy of the output.
"""

from __future__ import annotations

import argparse
import subprocess
import sys
import threading
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
LOG_DIR = ROOT / "target"

#: Seconds between two looks at a file that has no new bytes.
POLL_SECONDS = 0.05
#: Seconds `stop` waits for the tail to hand over the rest of the log.
STOP_SECONDS = 2.0
CHUNK = 64 * 1024


def add_arguments(parser: argparse.ArgumentParser) -> None:
    """The launcher's serial options."""
    parser.add_argument("--serial-log", metavar="PATH",
                        help="the guest's serial output, tailed to this console "
                             "(default: target/serial-<date>-<time>.log)")
    parser.add_argument("--sample-port", type=int, metavar="N",
                        help="a QMP socket on 127.0.0.1:N for tools/webplus/sample.py "
                             "(CR3 + RIP at 30 Hz during a freeze)")
    parser.add_argument("--serial-stdio", action="store_true",
                        help="the old single muxed stdio for serial and monitor "
                             "(a console that stops reading then stalls the guest)")


def _option_path(path: Path) -> str:
    """`path` as QEMU's option parser wants it: forward slashes, commas doubled."""
    return path.resolve().as_posix().replace(",", ",,")


def resolve_log(args: argparse.Namespace) -> Path:
    """The run's serial log: `--serial-log`, else a file named for this run
    (decided once, so the arguments and the tail agree)."""
    if not getattr(args, "_serial_log", None):
        args._serial_log = (Path(args.serial_log) if args.serial_log
                            else LOG_DIR / time.strftime("serial-%Y%m%d-%H%M%S.log"))
    return args._serial_log


def sample_args(port: int | None) -> list[str]:
    """The second QMP socket `tools/webplus/sample.py record --port N` reads."""
    return ["-qmp", f"tcp:127.0.0.1:{port},server=on,wait=off"] if port else []


def qemu_args(args: argparse.Namespace) -> list[str]:
    """The `-chardev`/`-serial`/`-mon` arguments for serial and monitor
    (and the sampling socket, if asked for)."""
    path = _option_path(resolve_log(args))
    if args.serial_stdio:
        chardevs = ["-chardev", f"stdio,id=ser0,mux=on,logfile={path}",
                    "-serial", "chardev:ser0", "-mon", "chardev=ser0,mode=readline"]
    else:
        chardevs = ["-chardev", f"file,id=ser0,path={path}", "-serial", "chardev:ser0",
                    "-chardev", "stdio,id=mon0,mux=on,signal=off",
                    "-mon", "chardev=mon0,mode=readline"]
    return chardevs + sample_args(args.sample_port)


class Tail:
    """Copy a growing file to `out` (default stdout) from a daemon thread."""

    def __init__(self, path: Path, out=None) -> None:
        self.path = Path(path)
        self.out = out if out is not None else getattr(sys.stdout, "buffer", sys.stdout)
        self._stop = threading.Event()
        self._thread = threading.Thread(target=self._run, name="serial-tail", daemon=True)
        self.copied = 0

    def start(self) -> None:
        self._thread.start()

    def stop(self) -> None:
        """Hand over what is left, then end; never wait on a blocked console."""
        self._stop.set()
        self._thread.join(STOP_SECONDS)

    def _write(self, data: bytes) -> None:
        try:
            self.out.write(data)
        except TypeError:
            self.out.write(data.decode("utf-8", "replace"))
        flush = getattr(self.out, "flush", None)
        if flush:
            flush()

    def _run(self) -> None:
        handle = None
        try:
            while True:
                stopping = self._stop.is_set()
                if handle is None:
                    try:
                        handle = open(self.path, "rb")
                    except OSError:
                        handle = None
                data = handle.read(CHUNK) if handle else b""
                if data:
                    self.copied += len(data)
                    self._write(data)
                    continue
                if stopping:
                    return
                time.sleep(POLL_SECONDS)
        except (OSError, ValueError):
            return  # the console went away: the log file still has everything
        finally:
            if handle:
                handle.close()


def prepare_log(log: Path) -> None:
    """Start the run with an empty log (QEMU truncates it, but the tail may look first)."""
    log.parent.mkdir(parents=True, exist_ok=True)
    try:
        log.unlink()
    except FileNotFoundError:
        pass
    except OSError:
        pass  # held open by something: QEMU truncates it when it opens it


def run(command: list[str], args: argparse.Namespace) -> int:
    """Run QEMU; with the file serial port, tail the log to this console."""
    log = resolve_log(args)
    print(f"serial log: {log}", flush=True)
    if args.serial_stdio:
        return subprocess.call(command)
    prepare_log(log)
    tail = Tail(log)
    tail.start()
    try:
        return subprocess.call(command)
    finally:
        tail.stop()
