"""The display mode as an image build switch (`LAZYOS_DISPLAY_MODE` ->
`display.mode` in `lazyos.cfg`; docs/hidpi-plan.md).

Shared by the GUI (`catalog.py`) and `tools/run_demo.py`. The kernel sets the
mode on QEMU's std VGA after boot (the BIOS bootloader stops at 1280x720),
and the desktop picks its UI scale from the screen: 2560x1440 is a 1280x720
desktop drawn at 2x. The image build re-validates the value
(`build_support/display_cfg.rs`), and the kernel checks it against the adapter.
"""

from __future__ import annotations

import re

#: The HiDPI preset: a 720p desktop at double density.
HIDPI_MODE = "2560x1440"
#: The mode range the kernel accepts (`kernel/src/display/modecfg.rs`).
MODE_MIN = (640, 480)
MODE_MAX = (3840, 2160)

_MODE = re.compile(r"^(\d{1,5})[xX](\d{1,5})$")


def check_mode(text: str) -> str:
    """The normalised ``WxH`` mode, or ``""`` for an empty one. Raises
    ``ValueError`` for anything the kernel would refuse."""
    text = (text or "").strip()
    if not text:
        return ""
    match = _MODE.match(text)
    if match:
        width, height = int(match.group(1)), int(match.group(2))
        if MODE_MIN[0] <= width <= MODE_MAX[0] and MODE_MIN[1] <= height <= MODE_MAX[1]:
            return f"{width}x{height}"
    raise ValueError(f"display mode {text!r}: expected WIDTHxHEIGHT between "
                     f"{MODE_MIN[0]}x{MODE_MIN[1]} and {MODE_MAX[0]}x{MODE_MAX[1]}")


def display_env(mode: str) -> dict[str, str]:
    """`LAZYOS_DISPLAY_MODE` for ``mode`` (none for an empty mode)."""
    mode = check_mode(mode)
    return {"LAZYOS_DISPLAY_MODE": mode} if mode else {}


def add_display_options(parser) -> None:
    """`--hidpi` and `--display-mode WxH` on an argparse parser."""
    parser.add_argument("--hidpi", action="store_true",
                        help=f"HiDPI: a {HIDPI_MODE} screen showing a 1280x720 desktop at 2x "
                             f"(same as --display-mode {HIDPI_MODE})")
    parser.add_argument("--display-mode", default="", metavar="WxH",
                        help="screen mode the kernel sets after boot (LAZYOS_DISPLAY_MODE, "
                             "written to lazyos.cfg); the desktop scale follows it")


def build_display(args) -> dict[str, str]:
    """The environment for `--hidpi`/`--display-mode`, refusing a mode a
    skipped build could never apply (it lives in `lazyos.cfg`)."""
    if args.hidpi and args.display_mode and check_mode(args.display_mode) != HIDPI_MODE:
        raise ValueError(f"--hidpi means --display-mode {HIDPI_MODE}")
    env = display_env(HIDPI_MODE if args.hidpi else args.display_mode)
    if env and args.no_build:
        raise ValueError("--hidpi/--display-mode need a build: the mode is written "
                         "into lazyos.cfg")
    return env
