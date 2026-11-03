"""The pairing-interface modes of `tools/bt/run.py` (docs/bluetooth-plan.md B5).

* `ui-pair`: a desktop image whose `btd` has **no pairing window** (the
  harness build with `LAZYOS_BT_PAIR=none`) opens Settings -> Bluetooth by
  clicking, scans, selects the LE keyboard that is in pairing mode, presses
  Pair, reads the passkey dialog (a screenshot), the keyboard's owner types the
  passkey (`fakebt` delays it so the dialog can be read), and the keyboard then
  types. Nothing may pair before the click (no silent pairing).
* `btctl`: the same pairing from the desktop Terminal with `btctl`: `list`,
  `scan`, `pair <address>` (prints the passkey), the paired device in `list`,
  `forget`, and `list` again. The Terminal reports the first output line of
  each command as `TERM:OUT:<line>`, so every command is wrapped to print one
  line (`BTCTL:<what>:<newlines turned into |>`).

The click targets are offsets from the centre of the Settings window's
content (`{"window": "Settings", "offset": [dx, dy]}`): the sidebar row, the
Scan and Pair buttons and the first list row of the 660x560 window
(`xui-app/crates/settings/tests/bluetooth.rs` leaves a snapshot to measure
them on).
"""

from __future__ import annotations

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import modes  # noqa: E402
import modes_le  # noqa: E402

UI_MODES = ("ui-pair",)
SHELL_MODES = ("btctl",)

#: The keyboard in pairing mode, and what it types once paired.
ADDRESS = modes_le.LE_KBD_ADDR
TEXT = "pairedui"
#: Milliseconds between the passkey being shown and its owner typing it: long
#: enough for a screenshot of the dialog and for `btctl`'s polling to see it.
PASSKEY_DELAY_MS = 5000

#: Offsets from the centre of the Settings window content (measured on a
#: desktop screenshot, `shot_ui_02_bluetooth.png`: the content is 662x561
#: dip at (49, 70), its centre (380, 350)).
SECTION_BLUETOOTH = (-255, 240)
SCAN = (-121, -24)
FIRST_ROW = (-90, -173)
#: While scanning the first button reads "Stop scanning" and the others move
#: right: this is where Pair is then.
PAIR = (-16, -24)

#: What the Settings window prints for the messages it handled
#: (`SETTINGS:MSG:<Debug of the message>`): the pairing events carry neither
#: the passkey nor a full address.
MSG = "SETTINGS:MSG:"
PAIRING = MSG + "Bt(Event(Pairing(BtPairing { phase: "


def image_env(mode: str) -> dict[str, str]:
    """The desktop harness image: no pairing window; the app that opens at
    login (Settings, or the Terminal for `btctl`); the UI probe names the
    windows and the label trace shows a missing permission."""
    return {
        "LAZYOS_DESKTOP": "1",
        "LAZYOS_XUI_AUTOSTART": "settings" if mode in UI_MODES else "term",
        "LAZYOS_UI_PROBE": "1",
        "LAZYOS_LABEL_TRACE": "1",
        "LAZYOS_AUTOLOGIN": "user",
        "LAZYOS_BT_TRACE": "1",
        "LAZYOS_BT_PAIR": "none",
    }


def fakebt_args(state: Path) -> list[str]:
    """One LE keyboard in pairing mode, typing after it is paired."""
    steps = [f"hid1+1500:type={TEXT}"]
    peers = ["--le-keyboard", ADDRESS, "--pairable", "--state", str(state),
             "--le-passkey-delay-ms", str(PASSKEY_DELAY_MS)]
    return peers + [arg for step in steps for arg in ("--at", step)]


def click(offset: tuple[int, int], until: str, **more) -> dict:
    return {"click_at": {"window": "Settings", "offset": list(offset)}, "until": until,
            "timeout": 60, "retries": 2, **more}


def ui_steps(timeout: float) -> list[dict]:
    """Open the pane, scan, select, pair, read the dialog, watch it type."""
    wait = lambda marker, **more: {"wait_for": marker, "timeout": timeout, **more}  # noqa: E731
    return [
        wait("SETTINGS:UP:PASS"),
        wait("BT:READY"),
        {"wait": 2.0},
        {"shot": "ui_01_settings"},
        click(SECTION_BLUETOOTH, MSG + "Section(9)"),
        wait(MSG + "Bt(Event(Snapshot"),
        {"shot": "ui_02_bluetooth"},
        click(SCAN, MSG + "Bt(Scan)"),
        wait(MSG + "Bt(Event(Device {"),
        {"wait": 1.0},
        click(FIRST_ROW, MSG + "Bt(Select(0))"),
        {"shot": "ui_03_found"},
        click(PAIR, "BT:API:PAIR"),
        wait(PAIRING + "ShowPasskey"),
        {"wait": 1.5},
        {"shot": "passkey_dialog"},
        wait(PAIRING + "Paired"),
        {"shot": "ui_05_paired"},
        wait(modes.last_key(TEXT)),
        {"wait": 3.0},
        {"quit": True},
    ]


#: The helper the session writes to `/tmp/r`: run a command, print one
#: `BTCTL:<what>:<status>:<output lines joined by |>` line. Each typed line is
#: short enough not to wrap (the Terminal's `TERM:OUT` matches the echo of a
#: command by its text, and a wrapped echo loses its trailing space).
HELPER = [
    r"""echo 'w=$1;shift;o=$("$@");c=$?' > /tmp/r""",
    r"""echo 'o=$(echo "$o"|tr "\n" "|")' >> /tmp/r""",
    r"""echo 'echo "BTCTL:$w:$c:$o"' >> /tmp/r""",
]


def btctl_steps(timeout: float) -> list[dict]:
    """The Terminal session: pair the keyboard with `btctl`, list, forget."""
    wait = lambda marker, **more: {"wait_for": marker, "timeout": timeout, **more}  # noqa: E731

    def command(text: str, until: str, patience: int = 60) -> list[dict]:
        return [{"type": text}, {"key": "enter", "until": until, "timeout": patience, "retries": 1}]

    def run(what: str, line: str) -> list[dict]:
        return command(f"sh /tmp/r {what} {line}", f"TERM:OUT:BTCTL:{what}:", 150)

    steps = [wait("TERM:UP:PASS"), wait("BT:READY"), {"wait": 2.0}]
    for line in HELPER:
        steps += command(line, "TERM:CMD:" + line)
    steps += run("LIST1", "btctl list")
    steps += run("SCAN", "btctl scan 8")
    steps += run("PAIR", f"btctl pair {ADDRESS}")
    # The keyboard's text landed on the Terminal's command line (the proof it
    # works); run it ("not found") so the next command starts on a clean line.
    steps += [wait(modes.last_key(TEXT)), {"wait": 1.0}, {"key": "enter"}, {"wait": 2.0}]
    steps += run("LIST2", "btctl list")
    steps += run("FORGET", f"btctl forget {ADDRESS}")
    steps += run("LIST3", "btctl list")
    steps += [{"shot": "btctl_done"}, {"wait": 3.0}, {"quit": True}]
    return steps


def session_steps(mode: str, timeout: float) -> list[dict]:
    return ui_steps(timeout) if mode in UI_MODES else btctl_steps(timeout)
