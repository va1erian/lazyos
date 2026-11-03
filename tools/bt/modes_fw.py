"""The firmware modes of `tools/bt/run.py --fw KIND` (docs/bluetooth-plan.md
B7): `fakebt` plays a radio that needs firmware instead of a generic dongle,
`btd` runs the vendor's loader against it with the real firmware files from
the image (fetched by `tools/firmware/fetch.py --switch bt`), and the pairing
test of `--pair` follows.

The simulated parts follow the documented download protocols (`libs/btsim`
`radio.rs`) but verify nothing: the point is that `btd` finds the part by its
USB ids, reads the file the part's own version answer names through
`libs/fwload`, sends every fragment, and carries on to the HID host.

* `intel`: an AX201 (`8087:0026`, legacy bootloader): `ibt-19-0-4.sfi`, then
  the `.ddc` configuration.
* `intel-tlv`: an AX211 (`8087:0033`, TLV version answer, both signature
  blocks): `ibt-0040-0041.sfi`.
* `intel-recovery`: an AX201 whose bootloader rejects the first image: `btd`
  sends the recovery reset, the dongle leaves the USB bus and returns, and
  the second attempt succeeds (`BT:FW:REENUM attempt=1`).
* `intel-reboot`: an AX201 that leaves the bus with the reset that boots the
  image and returns running it (`BT:FW:REENUM attempt=1`, then nothing to load).
* `realtek`: an RTL8822C (`0bda:c822`): `rtl8822cu_fw.bin`.
"""

from __future__ import annotations

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import modes  # noqa: E402

SFI, DDC = "intel/ibt-19-0-4.sfi", "intel/ibt-19-0-4.ddc"
RTL, RTL_CONFIG = "rtl_bt/rtl8822cu_fw.bin", "rtl_bt/rtl8822cu_config.bin"

#: kind -> (`fakebt` radio arguments, what the judge expects): the vendor and
#: USB ids `btd` names, the files it asks for in order, those among them the
#: image does not carry (optional ones), the re-enumerations, and whether the
#: last attempt downloaded.
KINDS: dict[str, tuple[list[str], dict]] = {
    "intel": (["--radio", "intel-ax201"],
              {"vendor": "intel", "usb": "8087:0026", "asked": [SFI, DDC], "absent": [], "reenum": 0,
               "downloaded": "1"}),
    "intel-tlv": (["--radio", "intel-ax211"],
                  {"vendor": "intel", "usb": "8087:0033",
                   "asked": ["intel/ibt-0040-0041.sfi", "intel/ibt-0040-0041.ddc"], "absent": [], "reenum": 0,
                   "downloaded": "1"}),
    # The first image is rejected: the recovery reset, the part leaves and
    # returns in its bootloader, and the second download succeeds.
    "intel-recovery": (["--radio", "intel-ax201", "--fw-reject", "1"],
                       {"vendor": "intel", "usb": "8087:0026", "asked": [SFI, SFI, DDC], "absent": [],
                        "reenum": 1, "downloaded": "1"}),
    # The reset that boots the image takes the part off the bus; it returns
    # running the image, so the second attempt finds nothing to download.
    "intel-reboot": (["--radio", "intel-ax201", "--fw-reboot"],
                     {"vendor": "intel", "usb": "8087:0026", "asked": [SFI], "absent": [], "reenum": 1,
                      "downloaded": "0"}),
    # linux-firmware has no board configuration file for this chip.
    "realtek": (["--radio", "rtl8822cu"],
                {"vendor": "realtek", "usb": "0bda:c822", "asked": [RTL, RTL_CONFIG], "absent": [RTL_CONFIG],
                 "reenum": 0, "downloaded": "1"}),
}


def fakebt_args(kind: str, state: Path) -> list[str]:
    """The radio first, then the peers and script of `--pair`."""
    return KINDS[kind][0] + modes.fakebt_args("pair", state)


def session_steps(timeout: float) -> list[dict]:
    """Wait for the download (a real image is thousands of commands), then the
    steps of `--pair`."""
    return [{"wait_for": "BT:FW:DONE", "timeout": timeout}] + modes.session_steps("pair", timeout)
