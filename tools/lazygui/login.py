"""The desktop's login (issue #623): `LAZYOS_AUTOLOGIN`, the account a
desktop image logs straight in instead of showing the login screen.

The image build reads the variable (`user/build.rs`): a login name logs that
account in through the same session path a typed password takes, `none`
shows the login screen, and unset leaves the image's default (the login
screen, except on an image that opens apps at login, `LAZYOS_XUI_AUTOSTART`,
which logs in `user`: the screenshot sessions' images). `run_demo.py
--autologin NAME` and the GUI (Simple: "Log in automatically as user";
Advanced: the "Autologin" field) set it; `run_demo.py` without the flag asks
for the login screen.

`LAZYOS_SETUP=1` (docs/accounts-plan.md U1, `run_demo.py --setup`, the GUI's
"First-boot setup") builds a desktop with no account at all: its login screen
asks for the owner, who becomes an administrator. The account database is a
seed an update never replaces, so the setup recreates the OS volume
(`LAZYOS_RESET_OS=1`), and it formats the attached home volume afresh with no
home at all (`--reset-home`): the owner may pick a name whose home an earlier
machine's account left there (review of #659, H6). An autologin image skips
the setup, so the two exclude each other here.
"""

from __future__ import annotations

import argparse
import re

#: The account the Simple tab's checkbox logs in.
DEFAULT_ACCOUNT = "user"
#: What the build reads as "show the login screen".
NONE = "none"
_NAME = re.compile(r"[a-z_][a-z0-9_-]{0,31}")


def check_name(name: str) -> str:
    """`name` stripped, if it is a login name (or empty); `ValueError` otherwise."""
    name = (name or "").strip()
    if name and name != NONE and not _NAME.fullmatch(name):
        raise ValueError(f"autologin {name!r} is not a login name ([a-z_][a-z0-9_-]*)")
    return name


def login_env(cfg: dict) -> dict[str, str]:
    """`LAZYOS_AUTOLOGIN` for a GUI build: the configured name, or nothing
    (the image's default) when none is set; the first-boot setup instead when
    asked for."""
    if cfg.get("setup"):
        return {"LAZYOS_SETUP": "1", "LAZYOS_AUTOLOGIN": NONE, "LAZYOS_RESET_OS": "1"}
    name = check_name(cfg.get("autologin", ""))
    return {"LAZYOS_AUTOLOGIN": name} if name else {}


def login_argv(cfg: dict) -> list[str]:
    """`run_demo.py`'s `--autologin NAME` for the configured name, or
    `--setup` (build switches, so nothing with "Skip build")."""
    if cfg.get("skip_build"):
        return []
    if cfg.get("setup"):
        return ["--setup"]
    name = check_name(cfg.get("autologin", ""))
    if not name or name == NONE:
        return []
    return ["--autologin", name]


def add_login_option(parser: argparse.ArgumentParser) -> None:
    """`--autologin NAME` on `run_demo.py`."""
    parser.add_argument("--autologin", metavar="NAME",
                        help="log the account NAME (e.g. user) straight into the desktop "
                             "instead of showing the login screen (LAZYOS_AUTOLOGIN; "
                             "default: the login screen; docs/accounts-plan.md U0)")
    parser.add_argument("--setup", action="store_true",
                        help="start with no account: the login screen asks for the owner, "
                             "an administrator (first-boot setup, LAZYOS_SETUP=1; recreates "
                             "the OS volume like --reset-os and formats the home volume "
                             "with no home, like --reset-home; docs/accounts-plan.md U1)")


def build_login(args: argparse.Namespace) -> dict[str, str]:
    """The environment `run_demo.py` builds with: the name, or `none` for the
    login screen. Nothing with `--no-build` (the image keeps what it has),
    where a name is an error."""
    name = check_name(args.autologin or "")
    setup = getattr(args, "setup", False)
    if args.no_build:
        if name or setup:
            raise ValueError("--autologin and --setup need a build: they are compiled into the image")
        return {}
    if setup:
        if name and name != NONE:
            raise ValueError("--setup asks for the owner at the login screen; it excludes --autologin")
        # The account database is a seed: only a new OS volume starts empty.
        # Its homes go too: a home volume made for other accounts would hand
        # their homes to whoever the owner's name matches (#659 H6).
        args.reset_os = True
        if not getattr(args, "no_home_disk", False):
            args.reset_home = True
        return {"LAZYOS_AUTOLOGIN": NONE, "LAZYOS_SETUP": "1"}
    return {"LAZYOS_AUTOLOGIN": name or NONE}
