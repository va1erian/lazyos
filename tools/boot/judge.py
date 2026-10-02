"""Judge one boot of the USB stick image from its serial log and screenshot.

Pure functions over text and pngstats numbers, so ``test_run.py`` can show the
judge fails when it should without booting anything.

The verdict (docs/usb-stick.md):

* ``BOOT:MEDIA:<word>`` is printed once and names the firmware the run used;
* ``FS:ROOT:<device>`` names a device matching the expected pattern
  (``ram0p\\d+`` for the stick: the RAM root, never a disk);
* the ready marker (the desktop shell, by default) appeared;
* ``usbd`` bound the USB keyboard (``USBD:HID:KBD``) and reported no fatal
  error: the target PC may have no PS/2 port, so the stick's input is USB;
* no kernel panic;
* the screenshot is not black and has enough colours to be a desktop.
"""

from __future__ import annotations

import re

MEDIA = re.compile(r"^BOOT:MEDIA:(\w+)\s*$", re.M)
ROOT = re.compile(r"^FS:ROOT:(\S+)\s*$", re.M)
PANIC = re.compile(r"LazyOS PANIC|panicked at")
USB_KBD = "USBD:HID:KBD"
USB_FATAL = re.compile(r"USBD:(FATAL|PANIC|PORT:FAIL|SLOT:LEAK)")

#: The default ready marker: LazyShell drew the desktop.
READY = "SHELL:DESKTOP:PASS"
#: The stick boots from its ramdisk partitions.
RAM_ROOT = r"ram0p\d+"
#: A desktop frame: mostly non-black and many colours (pngstats numbers).
MIN_NONBLACK = 0.30
MIN_COLOURS = 24


def judge_serial(log: str, firmware: str, root: str = RAM_ROOT,
                 ready: str | None = READY, usb_input: bool = True) -> list[str]:
    """Failures found in a serial log (empty when the boot passed)."""
    failures = []
    media = MEDIA.findall(log)
    if not media:
        failures.append("no BOOT:MEDIA line (the kernel did not start?)")
    elif len(media) > 1:
        failures.append(f"BOOT:MEDIA printed {len(media)} times (the machine rebooted?)")
    elif media[0] != firmware:
        failures.append(f"BOOT:MEDIA:{media[0]} but the run used {firmware}")
    roots = ROOT.findall(log)
    if not roots:
        failures.append("no FS:ROOT line")
    elif not re.fullmatch(root, roots[-1]):
        failures.append(f"FS:ROOT:{roots[-1]} does not match {root}")
    if ready and ready not in log:
        failures.append(f"{ready} never appeared")
    if usb_input and USB_KBD not in log:
        failures.append(f"{USB_KBD} never appeared (usbd missing, or no USB keyboard)")
    fatal = USB_FATAL.search(log)
    if usb_input and fatal:
        failures.append(f"usbd error: {fatal.group(0)}")
    if PANIC.search(log):
        failures.append("the kernel panicked")
    return failures


def judge_pixels(stats: dict, min_nonblack: float = MIN_NONBLACK,
                 min_colours: int = MIN_COLOURS) -> list[str]:
    """Failures for one screenshot's pngstats ``analyse`` result."""
    if "error" in stats:
        return [f"screenshot unreadable: {stats['error']}"]
    failures = []
    if stats["nonbackground_ratio"] < min_nonblack:
        failures.append(
            f"screen mostly black ({stats['nonbackground_ratio']} < {min_nonblack})")
    if stats["distinct_colors_q4"] < min_colours:
        failures.append(
            f"only {stats['distinct_colors_q4']} colours (< {min_colours}): not a desktop")
    return failures


def milestones(stamped: list[tuple[float, str]], ready: str | None = READY) -> dict:
    """Seconds from QEMU start to the first line of each milestone.

    ``stamped`` is ``(seconds, line)`` as the runner saw the serial log grow.
    The kernel line is when the bootloader finished loading the kernel and
    the ramdisk, which is where a large ramdisk costs time.
    """
    marks = {
        "kernel": "LazyOS: kernel entered",
        "media": "BOOT:MEDIA:",
        "root": "FS:ROOT:",
        "usb_keyboard": USB_KBD,
    }
    if ready:
        marks["ready"] = ready
    found = {}
    for name, needle in marks.items():
        for seconds, line in stamped:
            if needle in line:
                found[name] = round(seconds, 1)
                break
    return found
