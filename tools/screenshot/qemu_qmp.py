#!/usr/bin/env python3
"""Shared QEMU Machine Protocol (QMP) client with display capture and input
injection (keyboard + mouse).

Standard library only. The QMP socket is exposed over TCP (not a UNIX socket)
so the tooling works on Windows as well as Linux CI.

Used by ``qemu_shot.py`` (capture only) and ``qemu_session.py`` (scripted
interaction: type, click, capture).
"""

from __future__ import annotations

import json
import os
import shutil
import socket
import subprocess
import sys
import time
from pathlib import Path

# The volume options live beside this module; re-exported for the launchers.
from qemu_disks import (  # noqa: E402,F401
    add_data_disk_option, add_home_disk_option, data_disk_args, existing_data_disk,
    existing_home_disk, home_disk_args,
)

_WINDOWS_QEMU = (
    r"C:\Program Files\qemu\qemu-system-x86_64.exe",
    r"C:\Program Files (x86)\qemu\qemu-system-x86_64.exe",
)

# Guest RAM every launcher defaults to (`--memory`; docs/architecture/limits.md).
DEFAULT_MEMORY = "1G"


def find_qemu(explicit: str | None = None) -> str:
    """Locate qemu-system-x86_64 (--qemu, then PATH, then common dirs)."""
    if explicit:
        if not os.path.isfile(explicit):
            raise SystemExit(f"--qemu path does not exist: {explicit}")
        return explicit
    found = shutil.which("qemu-system-x86_64")
    if found:
        return found
    for candidate in _WINDOWS_QEMU:
        if os.path.isfile(candidate):
            return candidate
    raise SystemExit(
        "qemu-system-x86_64 not found. Install QEMU or pass --qemu PATH. "
        "(Windows: add 'C:\\Program Files\\qemu' to PATH)."
    )


def free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return int(s.getsockname()[1])


def _kvm_works(qemu: str) -> bool:
    """Start a paused QEMU with ``-accel kvm`` and ask it over QMP whether KVM
    is enabled, then quit. Catches hosts where ``/dev/kvm`` exists and is
    accessible but KVM still cannot initialise (e.g. no nested virtualization).
    """
    script = (
        '{"execute":"qmp_capabilities"}\n'
        '{"execute":"query-kvm"}\n'
        '{"execute":"quit"}\n'
    )
    try:
        proc = subprocess.run(
            [qemu, "-accel", "kvm", "-machine", "q35", "-display", "none",
             "-nodefaults", "-S", "-qmp", "stdio"],
            input=script, capture_output=True, text=True, timeout=20,
        )
    except Exception:
        return False
    return proc.returncode == 0 and '"enabled": true' in proc.stdout


def detect_accel(qemu: str) -> str | None:
    """Return the best hardware accelerator for this host, or None for TCG.

    WHPX on Windows and KVM on Linux make the guest run many times faster than
    TCG, which matters a lot for software rendering. KVM is only chosen if
    ``/dev/kvm`` is accessible *and* a real QEMU start with it succeeds, so an
    unusable KVM (missing, permission denied, no nested virt) falls back to TCG
    instead of failing the run.
    """
    try:
        proc = subprocess.run(
            [qemu, "-accel", "help"], capture_output=True, text=True, timeout=10
        )
        available = (proc.stdout + proc.stderr).lower()
    except Exception:
        return None
    if os.name == "nt" and "whpx" in available:
        return "whpx"
    if (
        "kvm" in available
        and os.path.exists("/dev/kvm")
        and os.access("/dev/kvm", os.R_OK | os.W_OK)
        and _kvm_works(qemu)
    ):
        return "kvm"
    return None


def resolve_accel(accel: str, qemu: str) -> str:
    """Resolve ``auto`` to a concrete accelerator name (``none`` for TCG)."""
    if accel == "auto":
        accel = detect_accel(qemu) or "none"
    return "none" if accel in ("tcg", "") else accel


def accel_args(accel: str, qemu: str) -> list[str]:
    """Resolve an ``--accel`` value into QEMU arguments (empty for TCG/none)."""
    requested = accel
    accel = resolve_accel(accel, qemu)
    if requested == "auto":
        print(f"qemu accelerator: auto -> {accel}", file=sys.stderr, flush=True)
    if accel == "none":
        return []
    if accel == "kvm":
        # KVM's in-kernel PIT defaults to re-injecting ticks the guest did not
        # acknowledge (e.g. while it ran with IF=0), delivering them in a
        # burst once interrupts are back on: the tick counter then jumps by
        # 100+ and tick-based sleeps look far too long. TCG's PIT drops such
        # ticks; `discard` gives KVM the same semantics.
        return ["-accel", "kvm", "-global", "kvm-pit.lost_tick_policy=discard"]
    return ["-accel", accel]


def build_qemu_command(
    qemu: str,
    image: str | None,
    qmp_port: int,
    serial_log: Path,
    memory: str = DEFAULT_MEMORY,
    extra_args: list[str] | None = None,
    data_disk: str | Path | None = None,
    ide: bool = False,
    home_disk: str | Path | None = None,
    boot_first: bool = False,
) -> list[str]:
    """Build a headless QEMU command line with a QMP socket and serial log.

    The boot ``image`` is attached as legacy virtio-blk (``disable-modern=on``,
    the interface the kernel drives), the way every launcher boots LazyOS; pass
    ``ide=True`` to attach it as IDE so the ATA driver is exercised instead.
    ``boot_first=True`` gives it ``bootindex=0``: firmware boots only the first
    hard disk it finds, and an extra IDE/AHCI disk (the ``--ahci`` scratch
    disk) can otherwise be enumerated ahead of the virtio boot disk.
    """
    command = [
        qemu,
        "-display", "none",
        "-no-reboot",
        "-qmp", f"tcp:127.0.0.1:{qmp_port},server=on,wait=off",
        "-serial", f"file:{serial_log.as_posix()}",
        "-m", memory,
    ]
    if image:
        file = Path(image).resolve().as_posix()
        if ide:
            command += ["-drive", f"format=raw,file={file}"]
        else:
            device = "virtio-blk-pci,drive=boot,disable-modern=on"
            if boot_first:
                device += ",bootindex=0"
            command += ["-drive", f"if=none,id=boot,format=raw,file={file}",
                        "-device", device]
    if data_disk:
        command += data_disk_args(data_disk)
    if home_disk:
        command += home_disk_args(home_disk)
    command += extra_args or []
    return command


class Qmp:
    """Minimal QMP client: handshake, execute, ignore async events."""

    def __init__(self, host: str, port: int, timeout: float):
        deadline = time.time() + timeout
        last_err: Exception | None = None
        self.sock: socket.socket | None = None
        # Buttons held through the monitor's `mouse_button` mask (drag & drop).
        self._held_buttons = 0
        # Least seconds between two calls that carry keys (`send_events`):
        # 0 under hardware acceleration, `TCG_KEY_INTERVAL` under TCG.
        self.key_interval = 0.0
        self._last_key_call = 0.0
        while time.time() < deadline:
            try:
                self.sock = socket.create_connection((host, port), timeout=2)
                break
            except OSError as exc:
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

    # ----- display -----------------------------------------------------
    def screenshot(self, dest: Path) -> Path:
        """Capture the primary display, preferring PNG; fall back to PPM."""
        png = dest.with_suffix(".png")
        try:
            self.execute("screendump", filename=png.as_posix(), format="png")
            return png
        except RuntimeError as exc:
            if "format" not in str(exc).lower():
                raise
            ppm = dest.with_suffix(".ppm")
            self.execute("screendump", filename=ppm.as_posix())
            return ppm

    # ----- input -------------------------------------------------------
    def send_events(self, events: list[dict], device: str | None = None) -> None:
        """Inject a list of QMP ``InputEvent`` objects.

        Key events are sent in calls that fit QEMU's 16-byte PS/2 queue
        (:func:`qemu_keys.ps2_batches`), with a pause between calls for the
        guest to drain it: one oversized call loses its tail inside QEMU.
        """
        for index, batch in enumerate(ps2_batches(events)):
            if index:
                time.sleep(PS2_DRAIN_SECONDS)
            if any(event.get("type") == "key" for event in batch):
                # Under TCG the guest drains the queue slower than QMP
                # fills it; `key_interval` spaces key calls out there.
                wait = self._last_key_call + self.key_interval - time.monotonic()
                if wait > 0:
                    time.sleep(wait)
                self._last_key_call = time.monotonic()
            arguments: dict = {"events": batch}
            if device:
                arguments["device"] = device
            self.execute("input-send-event", **arguments)

    def type_text(self, text: str, device: str | None = None, delay: float = 0.01) -> None:
        """Type ``text`` using a US keyboard layout.

        Events are paced with a small delay: injecting a whole string at once
        overruns QEMU's 16-byte i8042 output FIFO and silently drops keys.
        """
        for ch in text:
            self.send_events(char_events(ch), device)
            if delay:
                time.sleep(delay)

    def press_key(self, name: str, device: str | None = None) -> None:
        self.send_events(named_key_events(name), device)

    def key_down(self, name: str, device: str | None = None) -> None:
        """Press a named key and leave it held (chords: Alt+Tab, Ctrl+Esc)."""
        self.send_events(named_key_down_events(name), device)

    def key_up(self, name: str, device: str | None = None) -> None:
        """Release a key held by :meth:`key_down`."""
        self.send_events(named_key_up_events(name), device)

    def mouse_move(
        self, dx: int, dy: int, device: str | None = None, delay: float = 0.02
    ) -> None:
        """Move the pointer by (`dx`, `dy`), as paced single-packet steps.

        A PS/2 packet carries at most +-127 per axis, and QEMU's PS/2 mouse
        queue holds only 16 bytes. One large relative event needs several
        packets; the ones that do not fit are *deferred until the next input
        event* and then delivered after it, so a click sent right after a long
        move would fire before the pointer arrived (a wheel mouse's 4-byte
        packets make this happen at ~250 px instead of ~380 px). Each step here
        is one packet, and the small delay lets the guest drain the queue.
        """
        for step_x, step_y in mouse_move_steps(dx, dy):
            self.send_events(mouse_move_events(step_x, step_y), device)
            if delay:
                time.sleep(delay)

    def _monitor_buttons(self, mask: int) -> bool:
        """Set the pointer's held-button mask with the monitor's
        ``mouse_button`` command. Returns False when the monitor refuses it."""
        try:
            output = self.execute(
                "human-monitor-command", **{"command-line": f"mouse_button {mask}"}
            )
        except RuntimeError:
            return False
        # HMP reports its own errors (e.g. an unknown command) as output text in
        # a successful QMP reply; `mouse_button` prints nothing on success.
        return not (isinstance(output, str) and output.strip())

    def mouse_down(self, button: str = "left", device: str | None = None) -> None:
        """Press a button and hold it (drag & drop needs separate down/up).

        Uses the monitor ``mouse_button`` path for the same reason as
        :meth:`mouse_click`; falls back to the QMP event list when the button
        has no mask, a device is named, or the monitor refuses the command.
        """
        mask = _BUTTON_MASKS.get(button)
        if mask is not None and device is None:
            held = self._held_buttons | mask
            if self._monitor_buttons(held):
                self._held_buttons = held
                return
        self.send_events(mouse_button_events(button, True), device)

    def mouse_up(self, button: str = "left", device: str | None = None) -> None:
        """Release a held button (monitor path first, like :meth:`mouse_down`)."""
        mask = _BUTTON_MASKS.get(button)
        if mask is not None and device is None:
            held = self._held_buttons & ~mask
            if self._monitor_buttons(held):
                self._held_buttons = held
                return
        self.send_events(mouse_button_events(button, False), device)

    def mouse_click(self, button: str = "left", device: str | None = None) -> None:
        """Press and release a mouse button.

        ``input-send-event`` without an explicit device delivers to the first
        input handler, which is the keyboard: relative motion still reaches the
        mouse (only a pointer handles it), but a button press is accepted by
        the keyboard and dropped. The default PS/2 mouse has no QOM name to
        target, so the monitor's ``mouse_button`` command — which addresses the
        pointer directly — is the reliable path, with the QMP event list as a
        fallback for monitors without it.
        """
        mask = _BUTTON_MASKS.get(button)
        if mask is not None and device is None:
            held = self._held_buttons
            # Fall back only when the *press* fails: once it has landed, a
            # failed release must not trigger a second press.
            if self._monitor_buttons(held | mask):
                if not self._monitor_buttons(held):
                    self.send_events(mouse_button_events(button, False), device)
                return
        # The two transitions must be separate QMP calls: QEMU applies every
        # event of one input-send-event list to the legacy PS/2 device before
        # the guest drains it, so a down+up pair sent together nets out to no
        # click. Sending them separately makes the press visible.
        self.send_events(mouse_button_events(button, True), device)
        self.send_events(mouse_button_events(button, False), device)

    def mouse_scroll(self, amount: int, device: str | None = None) -> None:
        self.send_events(mouse_scroll_events(amount), device)

    def mouse_abs(self, x: int, y: int, device: str | None = None) -> None:
        self.send_events(mouse_abs_events(x, y), device)

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


# Keyboard events and PS/2 batching live beside this module (qemu_keys.py).
from qemu_keys import (  # noqa: E402,F401
    char_events, named_key_down_events, named_key_events, named_key_up_events,
    PS2_DRAIN_SECONDS, TCG_KEY_INTERVAL, ps2_batches,
)


# ---------------------------------------------------------------------------
# Mouse events
# ---------------------------------------------------------------------------

_BUTTONS = {"left", "middle", "right", "side", "extra", "wheel-up", "wheel-down"}
# The PS/2 button bitmask the monitor's `mouse_button` command takes.
# HMP `mouse_button` state bits, QEMU's MOUSE_EVENT_* values: 1 = left,
# 2 = right, 4 = middle.
_BUTTON_MASKS = {"left": 1, "right": 2, "middle": 4}


# The most a PS/2 packet moves the pointer along one axis.
PS2_MOUSE_STEP = 127


def mouse_move_steps(dx: int, dy: int) -> list[tuple[int, int]]:
    """Split a relative move into steps of at most one PS/2 packet each."""
    dx, dy = int(dx), int(dy)
    steps: list[tuple[int, int]] = []
    while dx or dy:
        step_x = max(-PS2_MOUSE_STEP, min(PS2_MOUSE_STEP, dx))
        step_y = max(-PS2_MOUSE_STEP, min(PS2_MOUSE_STEP, dy))
        steps.append((step_x, step_y))
        dx -= step_x
        dy -= step_y
    return steps


def mouse_move_events(dx: int, dy: int) -> list[dict]:
    events: list[dict] = []
    if dx:
        events.append({"type": "rel", "data": {"axis": "x", "value": int(dx)}})
    if dy:
        events.append({"type": "rel", "data": {"axis": "y", "value": int(dy)}})
    return events


def mouse_button_events(button: str = "left", down: bool = True) -> list[dict]:
    """One button transition, so a caller can hold a drag across moves."""
    if button not in _BUTTONS:
        raise ValueError(f"unknown mouse button {button!r}")
    return [{"type": "btn", "data": {"button": button, "down": bool(down)}}]


def mouse_click_events(button: str = "left") -> list[dict]:
    return mouse_button_events(button, True) + mouse_button_events(button, False)


def mouse_scroll_events(amount: int) -> list[dict]:
    button = "wheel-up" if amount > 0 else "wheel-down"
    events: list[dict] = []
    for _ in range(abs(int(amount))):
        events.append({"type": "btn", "data": {"button": button, "down": True}})
        events.append({"type": "btn", "data": {"button": button, "down": False}})
    return events


def mouse_abs_events(x: int, y: int) -> list[dict]:
    return [
        {"type": "abs", "data": {"axis": "x", "value": int(x)}},
        {"type": "abs", "data": {"axis": "y", "value": int(y)}},
    ]
