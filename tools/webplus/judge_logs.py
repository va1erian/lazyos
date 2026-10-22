"""Canned serial logs of a LazyWeb+ run, for the harness's own tests (`test_judge.py`, `test_quit_judge.py`).

The client's markers are `WEBPLUS:*`; the engine's are webd's relay `WEBD:NOTE:<line>`, beside webd's own
`WEBD:UP:PASS` and `WEBD:VIEW:*`.
"""

from __future__ import annotations

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import judge as j  # noqa: E402

P = j.P
#: webd relays the engine's markers behind this (`judge.engine_lines`).
N = j.NOTE
CREATE = f"WEBD:VIEW:CREATE client={j.CLIENT_LABEL} view=7"
DESTROY = "WEBD:VIEW:DESTROY view=7"


def log(*lines: str) -> str:
    return "\n".join(lines) + "\n"


def page(url: str, title: str) -> list[str]:
    return [f"{P}:LOAD:{url}", f"{P}:TITLE:{title}"]


def good_first() -> str:
    """A serial log that satisfies every serial judge of the first boot."""
    lines = [j.WEBD_UP, CREATE, f"{P}:UP:PASS", f"{N}PROCESS:LAUNCH:network:42", f"{N}PROCESS:LAUNCH:web:41",
             f"{N}NETWORKPROCESS:UP:42", f"{N}WEBPROCESS:UP:41"]
    for url, title in ((j.START, "LazyWeb+"), (j.EXAMPLE_URL, j.EXAMPLE_TITLE), (j.OLDNET_URL, j.OLDNET_TITLE),
                       (j.COOKIE_URL, j.COOKIE_NONE)):
        lines += page(url, title)
    lines += [f"{P}:DIALOG:alert", f"{P}:DIALOG:confirm", f"{P}:DIALOG:prompt"]
    lines += page(j.SCRIPT_URL, j.SCRIPT_TITLE)
    lines += [f"{P}:LAUNCH:{j.EVIL_MAILTO}:BLOCKED", f"{P}:LAUNCH:{j.COMMAND_URL}:BLOCKED"]
    lines += page(j.LINKS_URL, j.LINKS_TITLE)
    lines += [f"{P}:LAUNCH:{j.MAIL_URL}:FAIL"]
    lines += [f"{P}:POPUP:{j.BLANK_URL}", *page(j.BLANK_URL, j.BLANK_TITLE)]
    lines += [f"{P}:POPUP:{j.OPENED_URL}", *page(j.OPENED_URL, j.OPENED_TITLE)]
    lines += [f"{P}:DOWNLOAD:START:{j.DOWNLOAD_NAME}", f"{P}:DOWNLOAD:DONE:{j.DOWNLOAD_NAME}:{j.DOWNLOAD_BYTES}",
              f"{P}:DOWNLOAD:START:{j.SLOW_NAME}", f"{P}:DOWNLOAD:DONE:{j.SLOW_NAME}:{j.SLOW_BYTES}"]
    lines += page("about:history", "History") + page("about:downloads", "Downloads")
    lines += page(j.TALL_URL, j.TALL_TITLE) + page(j.LINKS_FORM_URL, j.LINKS_FORM_TITLE)
    lines += page(j.FORM_URL, j.FORM_TITLE) + [f"{P}:TITLE:{j.ECHO_TITLE}"]
    lines += [f"{P}:FAIL:Could not load {j.FAIL_URL}: Server returned nothing", f"{P}:STOPPED"]
    lines += [f"{P}:INFO:RESIZE:1000x400", f"{P}:INFO:RESIZE:1276x602"]
    # The window closes: webd destroys its view and the page's process ends; the network process is
    # webd's and stays.
    lines += [DESTROY, f"{N}PROCESS:EXIT:web:41:signal=9", f"{P}:QUIT:PASS"]
    return log(*lines)
