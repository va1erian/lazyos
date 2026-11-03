"""What each `tools/bt/run.py` mode runs: the peers `fakebt` plays, the script
they follow and the QEMU session that watches the serial log.

The image is one harness build (`LAZYOS_BT_TRACE=1`: `btd` gets `trace=1` and
the pairing window for the two peers below). The modes differ in what
`fakebt` is told: who is in pairing mode, what they type and when, and
whether they remember their keys from an earlier run (`--state`).
"""

from __future__ import annotations

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import judge_hid  # noqa: E402
import modes_le  # noqa: E402

KBD_ADDR = "C0:FF:EE:00:00:01"
MOUSE_ADDR = "C0:FF:EE:00:00:02"
#: A keyboard the host never paired with (`--refuse`).
INTRUDER_ADDR = "C0:FF:EE:00:00:09"
#: The pairing window the harness image opens (`LAZYOS_BT_PAIR`).
PAIR_LIST = f"{KBD_ADDR},{MOUSE_ADDR},{modes_le.LE_PAIR_LIST}"

#: What the keyboard types once paired, and what it types during the boot of
#: the reboot test and after the kill.
PAIR_TEXT = "hello"
BOOT_TEXT = "lazy"
RESTART_TEXT = "zq"
#: The key the kill test holds (`a`) and the crash exits on.
HELD_USAGE = 0x04


def last_key(text: str) -> str:
    """The `BT:KEY` line of the last release of `text`."""
    return f"BT:KEY usage={judge_hid.usage(text[-1]):#x} up"


def mouse_moves(trigger: str, start: int = 300) -> list[str]:
    """Two motions (the cursor starts mid-screen, so one cannot show it moved),
    then a button press and release, `start` ms after the mouse is connected.
    A reconnect at boot starts almost at once: the first report's delay after
    the link is what the boot-typing budget measures."""
    return [
        f"{trigger}+{start}:move=6,4,0",
        f"{trigger}+{start + 100}:move=-3,2,0",
        f"{trigger}+{start + 200}:move=0,0,1",
        f"{trigger}+{start + 400}:move=0,0,0",
    ]


def fakebt_args(mode: str, state: Path, boot: int = 1) -> list[str]:
    """The `fakebt` arguments of one boot of `mode`."""
    if mode in modes_le.LE_MODES:
        return modes_le.fakebt_args(mode, state, boot, PAIR_TEXT, BOOT_TEXT)
    peers = ["--keyboard", KBD_ADDR, "--mouse", MOUSE_ADDR, "--state", str(state)]
    steps: list[str] = []
    if mode == "pair":
        peers.append("--pairable")
        steps = [f"hid1+300:type={PAIR_TEXT}", *mouse_moves("mhid1")]
    elif mode == "reboot" and boot == 1:
        peers.append("--pairable")
        steps = [f"hid1+300:type={PAIR_TEXT}", *mouse_moves("mhid1")]
    elif mode == "reboot":
        # The user is typing while the machine boots; a bonded peer pages the
        # host and its keys wait for the link. Touching it again every few
        # seconds keeps it paging however long the boot takes.
        # The mouse's motion waits for its source: a report before the host
        # has the device back is lost, as it would be with a real mouse.
        steps = [f"200:type={BOOT_TEXT}", *mouse_moves("mhid1", start=20)]
        steps += [f"{n * 3000}:wake" for n in range(1, 40)]
    elif mode == "refuse":
        peers = ["--keyboard", INTRUDER_ADDR, "--forged-bond"]
        steps = ["armed1+500:type=ab", "armed1+2500:type=cd", "armed1+4500:type=ef"]
    elif mode == "kill":
        peers.append("--pairable")
        steps = [f"hid1+300:hold={HELD_USAGE:#x}", f"armed2+1000:type={RESTART_TEXT}"]
        steps += [f"armed2+{n * 3000}:wake" for n in range(1, 40)]
    return peers + [arg for step in steps for arg in ("--at", step)]


def session_steps(mode: str, timeout: float, boot: int = 1) -> list[dict]:
    """The QEMU session: wait for the evidence, let the last lines out, quit."""
    wait = lambda marker, **more: {"wait_for": marker, "timeout": timeout, **more}  # noqa: E731
    settle = [{"wait": 4.0}, {"quit": True}]
    if mode in modes_le.LE_MODES:
        return modes_le.session_steps(mode, timeout, boot, PAIR_TEXT, BOOT_TEXT, last_key)
    if mode == "pair" or (mode == "reboot" and boot == 1):
        steps = [wait("BT:HID:KBD"), wait("BT:HID:MOUSE"), wait(last_key(PAIR_TEXT))]
        return steps + settle
    if mode == "reboot":
        return [wait("BT:HID:KBD"), wait("BT:HID:MOUSE"), wait(last_key(BOOT_TEXT))] + settle
    if mode == "refuse":
        return [wait("BT:READY"), wait("BT:REFUSE reason=unbonded", occurrence=3)] + settle
    if mode == "kill":
        return [
            wait("BT:HID:KBD"),
            wait("BT:CRASH"),
            wait("BT:READY", occurrence=2),
            wait(last_key(RESTART_TEXT)),
        ] + settle
    return [wait("BT:CTRL:PASS")] + settle
