"""The LE modes of `tools/bt/run.py` (docs/bluetooth-plan.md B4): the LE peers
`fakebt` plays, the script they follow and the QEMU session that watches the
serial log. The shared pieces (addresses, texts, the mouse script) are
`modes.py`'s.

* `le-pair`: an LE keyboard (Passkey Entry) and an LE mouse (Just Works) pair
  and work.
* `le-reboot`: the same, then the same image boots again and both reconnect
  by themselves (the host arms the accept list first), the keyboard typing
  while the machine boots.
* `le-rpa`: as `le-reboot`, but the bonded devices change their address each
  time they advertise, so the host has to resolve it with the IRK; the
  keyboard disconnects and comes back once in the second boot.
* `le-refuse`: a Just Works LE keyboard is in the pairing list and is
  refused once it turns out to be a keyboard; an LE keyboard with a key the
  host never issued types and is never connected.
"""

from __future__ import annotations

from pathlib import Path

LE_KBD_ADDR = "C0:FF:EE:00:01:01"
LE_MOUSE_ADDR = "C0:FF:EE:00:01:02"
#: A keyboard that can only do Just Works (`le-refuse`).
LE_JW_ADDR = "C0:FF:EE:00:01:03"
#: A keyboard the host never paired with (`le-refuse`).
LE_INTRUDER_ADDR = "C0:FF:EE:00:01:09"
#: What the pairing list of the harness image adds (`le:` entries).
LE_PAIR_LIST = f"le:{LE_KBD_ADDR},le:{LE_MOUSE_ADDR},le:{LE_JW_ADDR}"

#: What the keyboard types in the `le-rpa` second boot: before and after it
#: drops the link and comes back under a new address.
RPA_TEXT_AFTER = "go"

LE_MODES = ("le-pair", "le-reboot", "le-rpa", "le-refuse")
#: Modes that boot twice on one image.
TWO_BOOTS = ("le-reboot", "le-rpa")


def mouse_moves(when: str) -> list[str]:
    """Two motions, then a button press and release; `when` is the prefix of
    each time (`mhid1+` after the mouse is ready, empty after connecting)."""
    return [
        f"{when}300:move=6,4,0",
        f"{when}400:move=-3,2,0",
        f"{when}500:move=0,0,1",
        f"{when}700:move=0,0,0",
    ]


def fakebt_args(mode: str, state: Path, boot: int, pair_text: str, boot_text: str) -> list[str]:
    """The `fakebt` arguments of one boot of the LE mode `mode`."""
    if mode == "le-refuse":
        peers = ["--le-keyboard", LE_JW_ADDR, "--le-jw-keyboard", "--le-intruder", LE_INTRUDER_ADDR,
                 "--state", str(state), "--pairable"]
        steps = ["armed1+500:itype=ab", "armed1+1500:type=zz", "armed1+2500:itype=cd", "armed1+4500:itype=ef"]
        return peers + [arg for step in steps for arg in ("--at", step)]
    peers = ["--le-keyboard", LE_KBD_ADDR, "--le-mouse", LE_MOUSE_ADDR, "--state", str(state)]
    if mode == "le-rpa":
        peers.append("--le-rpa")
    pairing = boot == 1
    if pairing:
        peers.append("--pairable")
        steps = [f"hid1+300:type={pair_text}", *mouse_moves("mhid1+")]
    else:
        # The user is typing while the machine boots; a bonded peer
        # advertises when touched, so it is touched again every few seconds.
        steps = [f"200:type={boot_text}", *mouse_moves("")]
        steps += [f"{n * 3000}:wake" for n in range(1, 40)]
        if mode == "le-rpa":
            steps += ["hid1+1500:dropkbd=0x13", f"hid1+2500:type={RPA_TEXT_AFTER}"]
    return peers + [arg for step in steps for arg in ("--at", step)]


def session_steps(mode: str, timeout: float, boot: int, pair_text: str, boot_text: str,
                  last_key) -> list[dict]:
    """The QEMU session: wait for the evidence, let the last lines out, quit."""
    wait = lambda marker, **more: {"wait_for": marker, "timeout": timeout, **more}  # noqa: E731
    settle = [{"wait": 4.0}, {"quit": True}]
    if mode == "le-refuse":
        return [wait("BT:READY"), wait("BT:REFUSE reason=keyboard-unauthenticated"), {"wait": 10.0},
                {"quit": True}]
    if boot == 1:
        return [wait("BT:HID:KBD"), wait("BT:HID:MOUSE"), wait(last_key(pair_text))] + settle
    final = RPA_TEXT_AFTER if mode == "le-rpa" else boot_text
    return [wait("BT:HID:KBD"), wait("BT:HID:MOUSE"), wait(last_key(final))] + settle
