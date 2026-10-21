"""The display mode and UI scale as image build switches (`LAZYOS_DISPLAY_MODE`
-> `display.mode`, `LAZYOS_DISPLAY_SCALE` -> `display.scale` in `lazyos.cfg`;
docs/hidpi-plan.md).

Shared by the GUI (`catalog.py`) and `tools/run_demo.py`. The kernel sets the
mode on QEMU's std VGA after boot (the BIOS bootloader stops at 1280x720),
and the desktop picks its UI scale from the screen unless the image says
otherwise: 2560x1440 is a 1280x720 desktop drawn at 2x (HiDPI), and with the
scale at 1 the same screen is a roomy 2560x1440 desktop whose text and
widgets keep their normal size (hi-res). The image build re-validates the
values (`build_support/display_cfg.rs`), and the kernel checks the mode
against the adapter.
"""

from __future__ import annotations

import re

#: The HiDPI preset: a 720p desktop at double density.
HIDPI_MODE = "2560x1440"
#: The hi-res preset: the same screen, a 1440p desktop at scale 1.
HIRES_MODE = HIDPI_MODE
HIRES_SCALE = "1"
#: The UI scales `display.scale` takes (`build_support/display_cfg.rs`).
SCALES = ("auto", "1", "2")
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


def check_scale(text: str) -> str:
    """The normalised UI scale (``auto``, ``1`` or ``2``), or ``""`` for an
    empty one (the image's default: auto). Raises ``ValueError`` for anything
    the image build would refuse."""
    text = (text or "").strip().lower()
    if text and text not in SCALES:
        raise ValueError(f"display scale {text!r}: expected auto, 1 or 2")
    return text


def display_env(mode: str, max_size: str = "", scale: str = "") -> dict[str, str]:
    """`LAZYOS_DISPLAY_MODE` for ``mode``, `LAZYOS_DISPLAY_SCALE` for ``scale``
    and `LAZYOS_DISPLAY_MAX` for ``max_size`` (none for an empty one; the
    2560x1440 mode brings its own cap)."""
    env = {}
    mode = check_mode(mode)
    if mode:
        env["LAZYOS_DISPLAY_MODE"] = mode
    scale = check_scale(scale)
    if scale:
        env["LAZYOS_DISPLAY_SCALE"] = scale
    max_size = check_mode(max_size)
    if mode == HIDPI_MODE and not max_size:
        # `display.mode` only switches a Bochs adapter (QEMU); a real PC keeps
        # its firmware framebuffer, which `display.max` caps at 1920x1080 unless
        # raised (issue #717), so HiDPI and hi-res ask for both.
        max_size = HIDPI_MODE
    if max_size:
        env["LAZYOS_DISPLAY_MAX"] = max_size
    return env


def add_display_options(parser) -> None:
    """`--hidpi`, `--hires`, `--display-mode WxH`, `--display-scale` and
    `--display-max WxH` on an argparse parser."""
    parser.add_argument("--hidpi", action="store_true",
                        help=f"HiDPI: a {HIDPI_MODE} screen showing a 1280x720 desktop at 2x "
                             f"(same as --display-mode {HIDPI_MODE} --display-scale 2)")
    parser.add_argument("--hires", action="store_true",
                        help=f"Hi-res: a {HIRES_MODE} screen showing a roomy {HIRES_MODE} "
                             "desktop at 1x, text and widgets at their normal size "
                             f"(same as --display-mode {HIRES_MODE} --display-scale 1)")
    parser.add_argument("--display-mode", default="", metavar="WxH",
                        help="screen mode the kernel sets after boot (LAZYOS_DISPLAY_MODE, "
                             "written to lazyos.cfg); the desktop scale follows it")
    parser.add_argument("--display-scale", default="", choices=("",) + SCALES, metavar="{auto,1,2}",
                        help="UI scale of the desktop and the boot console (LAZYOS_DISPLAY_SCALE, "
                             "display.scale; default auto: 2 from 2560x1440, else 1)")
    parser.add_argument("--display-max", default="", metavar="WxH",
                        help="logical screen cap for a firmware framebuffer on a real PC "
                             "(LAZYOS_DISPLAY_MAX, display.max; default 1920x1080): "
                             f"{HIDPI_MODE} gives a 1280x720 desktop at 2x (--display-scale 1: "
                             "a 2560x1440 desktop)")


def build_display(args) -> dict[str, str]:
    """The environment for `--hidpi`/`--hires`/`--display-mode`/`--display-scale`,
    refusing a combination that contradicts itself and any of them with no
    build (they live in `lazyos.cfg`)."""
    if args.hidpi and args.hires:
        raise ValueError("--hidpi and --hires exclude each other: HiDPI is a 1280x720 desktop at "
                         "2x (sharp, big text), hi-res is a 2560x1440 desktop at 1x (roomy, "
                         "normal-size text); pick one")
    mode, scale = args.display_mode, args.display_scale
    #: flag -> (the scales it allows, the scale it sets, the flag to use instead)
    presets = {"hidpi": (("", "auto", "2"), "", "--hires"), "hires": (("", "1"), HIRES_SCALE, "--hidpi")}
    for name, (allowed, sets, instead) in presets.items():
        if not getattr(args, name):
            continue
        if mode and check_mode(mode) != HIRES_MODE:
            raise ValueError(f"--{name} means --display-mode {HIRES_MODE}")
        if scale not in allowed:
            raise ValueError(f"--{name} cannot take --display-scale {scale} (use {instead}, or "
                             "--display-mode and --display-scale on their own)")
        mode, scale = HIRES_MODE, scale or sets
    env = display_env(mode, args.display_max, scale)
    if env and args.no_build:
        raise ValueError("--hidpi/--hires/--display-mode/--display-scale/--display-max need a "
                         "build: they are written into lazyos.cfg")
    return env


def link_exclusive(v: dict, names=("simple_hidpi", "simple_hires")) -> None:
    """Make the Simple tab's HiDPI and Hi-res checkboxes mutually exclusive:
    ticking one clears the other. ``v`` is the Tk variable table."""
    for name, other in (names, names[::-1]):
        def cleared(*_args, name=name, other=other) -> None:
            if v[name].get() and v[other].get():
                v[other].set(False)
        v[name].trace_add("write", cleared)
