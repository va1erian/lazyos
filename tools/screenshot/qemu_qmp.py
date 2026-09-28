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
import time
from pathlib import Path

_WINDOWS_QEMU = (
    r"C:\Program Files\qemu\qemu-system-x86_64.exe",
    r"C:\Program Files (x86)\qemu\qemu-system-x86_64.exe",
)


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


def detect_accel(qemu: str) -> str | None:
    """Return the best hardware accelerator for this host, or None for TCG.

    WHPX on Windows and KVM on Linux make the guest run many times faster than
    TCG, which matters a lot for software rendering.
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
    ):
        return "kvm"
    return None


def accel_args(accel: str, qemu: str) -> list[str]:
    """Resolve an ``--accel`` value into QEMU arguments (empty for TCG/none)."""
    if accel == "auto":
        accel = detect_accel(qemu) or "none"
    if accel in ("none", "tcg", ""):
        return []
    return ["-accel", accel]


def build_qemu_command(
    qemu: str,
    image: str | None,
    qmp_port: int,
    serial_log: Path,
    memory: str = "256M",
    extra_args: list[str] | None = None,
) -> list[str]:
    """Build a headless QEMU command line with a QMP socket and serial log."""
    command = [
        qemu,
        "-display", "none",
        "-no-reboot",
        "-qmp", f"tcp:127.0.0.1:{qmp_port},server=on,wait=off",
        "-serial", f"file:{serial_log.as_posix()}",
        "-m", memory,
    ]
    if image:
        command += ["-drive", f"format=raw,file={Path(image).resolve().as_posix()}"]
    command += extra_args or []
    return command


class Qmp:
    """Minimal QMP client: handshake, execute, ignore async events."""

    def __init__(self, host: str, port: int, timeout: float):
        deadline = time.time() + timeout
        last_err: Exception | None = None
        self.sock: socket.socket | None = None
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
        """Inject a list of QMP ``InputEvent`` objects."""
        if not events:
            return
        arguments: dict = {"events": events}
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

    def mouse_move(self, dx: int, dy: int, device: str | None = None) -> None:
        self.send_events(mouse_move_events(dx, dy), device)

    def mouse_down(self, button: str = "left", device: str | None = None) -> None:
        self.send_events(mouse_button_events(button, True), device)

    def mouse_up(self, button: str = "left", device: str | None = None) -> None:
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
            try:
                self.execute(
                    "human-monitor-command", **{"command-line": f"mouse_button {mask}"}
                )
                self.execute("human-monitor-command", **{"command-line": "mouse_button 0"})
                return
            except RuntimeError:
                pass
        # The two transitions must be separate QMP calls: QEMU applies every
        # event of one input-send-event list to the legacy PS/2 device before
        # the guest drains it, so a down+up pair sent together nets out to no
        # click. Sending them separately makes the press visible.
        self.mouse_down(button, device)
        self.mouse_up(button, device)

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


# ---------------------------------------------------------------------------
# Keyboard layout (US) -> QKeyCode
# ---------------------------------------------------------------------------

_SHIFT = "shift"

# Characters that live on the top row unshifted.
_UNSHIFTED = {
    "`": "grave_accent", "-": "minus", "=": "equal", "[": "bracket_left",
    "]": "bracket_right", "\\": "backslash", ";": "semicolon", "'": "apostrophe",
    ",": "comma", ".": "dot", "/": "slash",
}
# Characters that require Shift.
_SHIFTED = {
    "~": "grave_accent", "_": "minus", "+": "equal", "{": "bracket_left",
    "}": "bracket_right", "|": "backslash", ":": "semicolon", '"': "apostrophe",
    "<": "comma", ">": "dot", "?": "slash",
    "!": "1", "@": "2", "#": "3", "$": "4", "%": "5",
    "^": "6", "&": "7", "*": "8", "(": "9", ")": "0",
}
_NAMED = {
    "enter": "ret", "return": "ret", "ret": "ret",
    "esc": "esc", "escape": "esc",
    "space": "spc", "spc": "spc", "tab": "tab",
    "backspace": "backspace", "delete": "delete", "del": "delete",
    "insert": "insert", "home": "home", "end": "end",
    "pageup": "pgup", "pagedown": "pgdn",
    "up": "up", "down": "down", "left": "left", "right": "right",
    "shift": "shift", "ctrl": "ctrl", "control": "ctrl", "alt": "alt",
    "capslock": "caps_lock", "menu": "menu",
}


def _key_event(qcode: str, down: bool) -> dict:
    return {"type": "key", "data": {"down": down, "key": {"type": "qcode", "data": qcode}}}


def _key_seq(qcode: str, shift: bool) -> list[dict]:
    events: list[dict] = []
    if shift:
        events.append(_key_event(_SHIFT, True))
    events.append(_key_event(qcode, True))
    events.append(_key_event(qcode, False))
    if shift:
        events.append(_key_event(_SHIFT, False))
    return events


def char_events(ch: str) -> list[dict]:
    """Return the key events needed to type a single character."""
    if ch == "\n":
        return _key_seq("ret", False)
    if ch == "\t":
        return _key_seq("tab", False)
    if ch == " ":
        return _key_seq("spc", False)
    if "a" <= ch <= "z" or "0" <= ch <= "9":
        return _key_seq(ch, False)
    if "A" <= ch <= "Z":
        return _key_seq(ch.lower(), True)
    if ch in _SHIFTED:
        return _key_seq(_SHIFTED[ch], True)
    if ch in _UNSHIFTED:
        return _key_seq(_UNSHIFTED[ch], False)
    raise ValueError(f"cannot type character {ch!r}")


def named_key_events(name: str) -> list[dict]:
    """Return key events for a named key (e.g. ``enter``, ``esc``, ``f5``)."""
    lowered = name.lower()
    if lowered in _NAMED:
        return _key_seq(_NAMED[lowered], False)
    if len(name) == 1:
        return char_events(name)
    if lowered.startswith("f") and lowered[1:].isdigit() and 1 <= int(lowered[1:]) <= 12:
        return _key_seq(lowered, False)
    raise ValueError(f"unknown key name {name!r}")


# ---------------------------------------------------------------------------
# Mouse events
# ---------------------------------------------------------------------------

_BUTTONS = {"left", "middle", "right", "side", "extra", "wheel-up", "wheel-down"}
# The PS/2 button bitmask the monitor's `mouse_button` command takes.
_BUTTON_MASKS = {"left": 1, "middle": 2, "right": 4}


def mouse_move_events(dx: int, dy: int) -> list[dict]:
    events: list[dict] = []
    if dx:
        events.append({"type": "rel", "data": {"axis": "x", "value": int(dx)}})
    if dy:
        events.append({"type": "rel", "data": {"axis": "y", "value": int(dy)}})
    return events


def mouse_button_events(button: str = "left", down: bool = True) -> list[dict]:
    if button not in _BUTTONS:
        raise ValueError(f"unknown mouse button {button!r}")
    return [{"type": "btn", "data": {"button": button, "down": bool(down)}}]


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
