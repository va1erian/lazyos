#!/usr/bin/env python3
"""Keyboard events for QMP ``input-send-event``: US characters and named keys
to QKeyCodes, and the batching that keeps QEMU's PS/2 queue from overflowing.

Split out of ``qemu_qmp.py`` (which re-exports everything here).
"""

from __future__ import annotations

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
    # The Super/Windows/GUI key is QEMU's left "meta" key.
    "super": "meta_l", "meta": "meta_l", "win": "meta_l", "gui": "meta_l",
    "capslock": "caps_lock", "menu": "menu",
    # Right-hand modifiers (AltGr on AZERTY), lock keys and the keypad, for
    # the input subsystem's scripted checks (docs/input-plan.md).
    "altgr": "alt_r", "alt_r": "alt_r", "ctrl_r": "ctrl_r", "shift_r": "shift_r",
    "super_r": "meta_r", "numlock": "num_lock", "scrolllock": "scroll_lock",
    "print": "print", "pause": "pause",
    "kp_add": "kp_add", "kp_subtract": "kp_subtract", "kp_multiply": "kp_multiply",
    "kp_divide": "kp_divide", "kp_decimal": "kp_decimal", "kp_enter": "kp_enter",
    **{f"kp_{n}": f"kp_{n}" for n in range(10)},
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


def named_key_down_events(name: str) -> list[dict]:
    """One press, so the caller can hold the key across later steps."""
    return [_key_event(_hold_qcode(name), True)]


def named_key_up_events(name: str) -> list[dict]:
    """Release half of :func:`named_key_down_events`."""
    return [_key_event(_hold_qcode(name), False)]


def _hold_qcode(name: str) -> str:
    """Map a holdable key name to a QKeyCode.

    Only named keys and unshifted characters are supported: a shifted symbol
    would need the Shift modifier held too, which ``key_down``/``key_up`` do
    not express.
    """
    lowered = name.lower()
    if lowered in _NAMED:
        return _NAMED[lowered]
    if len(name) == 1 and ("a" <= name <= "z" or "0" <= name <= "9"):
        return name
    if name in _UNSHIFTED:
        return _UNSHIFTED[name]
    if lowered.startswith("f") and lowered[1:].isdigit() and 1 <= int(lowered[1:]) <= 12:
        return lowered
    raise ValueError(f"cannot hold key {name!r}")


# ---------------------------------------------------------------------------
# PS/2 batching (issue #400)
# ---------------------------------------------------------------------------

# QEMU's PS/2 keyboard queues at most 16 bytes and silently discards the rest.
# Every event of one ``input-send-event`` call is queued before the guest can
# read a byte, so a call whose events need more than this loses its tail
# inside QEMU (measured with ``-trace ps2_put_keycode,pckbd_kbd_read_data``:
# 12 keys in one call queued 24 bytes and the guest could read 16).
PS2_QUEUE_BYTES = 16

# How long the guest gets to drain the queue between two batches (its
# interrupts-off stretches stay under a few milliseconds).
PS2_DRAIN_SECONDS = 0.01

# Under TCG the emulated CPU drains the queue far slower than QMP fills it
# (measured: unpaced per-character calls lost 12 to 28 of 658 bytes inside
# QEMU), so launchers space key-carrying calls this far apart there.
TCG_KEY_INTERVAL = 0.01

# Scancode-set-1 bytes one key event costs at most: 2 for an ``E0`` key,
# Pause's 6-byte make (it has no break), PrintScreen's 4.
_PS2_COST = {"pause": 6, "print": 4, "sysrq": 4}


def _ps2_cost(event: dict) -> int:
    """Bytes ``event`` puts in the PS/2 keyboard queue (0 for the mouse)."""
    if event.get("type") != "key":
        return 0
    qcode = event.get("data", {}).get("key", {}).get("data", "")
    return _PS2_COST.get(qcode, 2)


def ps2_batches(events: list[dict]) -> list[list[dict]]:
    """Split ``events`` into calls whose keyboard bytes each fit QEMU's PS/2
    queue (the caller lets the guest drain between them). Order is kept, and
    a list that fits stays one call."""
    batches: list[list[dict]] = []
    current: list[dict] = []
    used = 0
    for event in events:
        cost = _ps2_cost(event)
        if current and used + cost > PS2_QUEUE_BYTES:
            batches.append(current)
            current, used = [], 0
        current.append(event)
        used += cost
    if current:
        batches.append(current)
    return batches
