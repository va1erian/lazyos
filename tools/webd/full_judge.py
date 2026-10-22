"""The judge of the full-view harness (`full_run.py`): the test client's markers (`WEBDBROWSE:*`, one per
step of its scenario, `xui-app/crates/webd-client/src/browse/`), what the host's server saw
(`full_server.py`), the file the download left, the label trace and a screenshot.

Every function returns a list of problems; an empty list passes. Each is tested to fail when it should
(`test_full_judge.py`).
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import judge as light  # noqa: E402  (the light harness's: label trace, rectangles, PNG reading)

B = "WEBDBROWSE"
W = "WEBD"
WINDOW = "Web View Test"
#: The scenario's steps, in the order the client runs them.
STEPS = ["VIEW", "BLOCKLIST", "NAV", "BACK", "FORWARD", "BLOCKED", "UNBLOCKED", "COOKIE_A", "COOKIE_B",
         "COOKIE_A2", "DOWNLOAD", "DIALOG", "POPUP", "HELD", "SCOPE", "DELTA", "LATENCY"]
STEP = re.compile(rf"^{B}:([A-Z_0-9]+):(PASS|FAIL)(?::([^\r\n]*))?", re.M)
LATENCY = re.compile(r"load_ms=(\d+) wheel_ms min=(\d+) median=(\d+) max=(\d+) n=(\d+) lost=(\d+) "
                     r"refresh_ms min=(\d+) median=(\d+) max=(\d+)")
DOWNLOAD = re.compile(rf"^{B}:DOWNLOAD:PASS:name=(\S+) bytes=(\d+)", re.M)
#: Labels this harness answers for: the test client, webd and webd's children.
OWN = re.compile(r"app:os\.lazy\.webd(browse)?(/\S+)?$")


def steps(text: str) -> dict[str, tuple[str, str]]:
    """The last verdict and detail of each step the client printed."""
    return {m[1]: (m[2], m[3] or "") for m in STEP.finditer(text)}


def judge_service(text: str) -> list[str]:
    problems = []
    if f"{W}:SERVE:app.os.lazy.webd.view" not in text:
        problems.append("webd never said it serves app.os.lazy.webd.view")
    if f"{B}:UP:PASS" not in text:
        problems.append("the test client never came up")
    return problems


def judge_steps(text: str) -> list[str]:
    seen = steps(text)
    problems = [f"{name}: {seen[name][1] or 'FAIL'}" for name in STEPS if name in seen and seen[name][0] != "PASS"]
    problems += [f"{name}: never reported" for name in STEPS if name not in seen]
    done = seen.get("DONE")
    if done is None:
        problems.append("the scenario never finished (no WEBDBROWSE:DONE)")
    elif done[0] != "PASS":
        problems.append(f"DONE:FAIL {done[1]}")
    return problems


def requests_of(record: dict, path: str) -> list[dict]:
    return [r for r in record.get("requests", []) if r.get("path") == path]


def judge_blocking(record: dict) -> list[str]:
    """`/blocked.png` never reached the server while the Index page was shown with the blocker on (between
    the first `/` and the first `/page2`), and did reach it on the reload that bypassed the blocker."""
    paths = [r["path"] for r in record.get("requests", [])]
    if "/" not in paths or "/page2" not in paths:
        return ["the server never saw the Index page and then Page Two"]
    first, second = paths.index("/"), paths.index("/page2")
    problems = []
    if "/blocked.png" in paths[first:second]:
        problems.append("the blocked picture was requested while the blocker was on")
    if "/pic.png" not in paths[first:second]:
        problems.append("the Index page's other picture was not requested (the control)")
    if "/blocked.png" not in paths[second:]:
        problems.append("the reload without the blocker did not request the blocked picture")
    return problems


def judge_cookies(record: dict) -> list[str]:
    """What each profile sent to `/cookie`, in the scenario's order: the browser profile its own cookie, the
    app's own profile none of it (then its own), the browser profile again only its own."""
    cookies = [r.get("cookie", "") for r in requests_of(record, "/cookie")]
    if len(cookies) < 4:
        return [f"the server saw {len(cookies)} request(s) of /cookie, the scenario makes 4"]
    a, b1, b2, a2 = cookies[:4]
    problems = []
    if "webd=browser" not in a:
        problems.append(f"the browser profile did not send its cookie: {a!r}")
    if "webd=browser" in b1:
        problems.append(f"the app's own profile sent the browser profile's cookie: {b1!r}")
    if "webd=own" not in b2 or "webd=browser" in b2:
        problems.append(f"the app's own profile did not keep its cookie apart: {b2!r}")
    if "webd=browser" not in a2 or "webd=own" in a2:
        problems.append(f"a second view of the browser profile did not see only its cookie: {a2!r}")
    return problems


def judge_network(record: dict) -> list[str]:
    """A view without the network scope reached nothing: `/nonet.png` was never asked for."""
    if requests_of(record, "/nonet.png"):
        return ["the view made without the network scope reached the server (/nonet.png)"]
    return []


def judge_download(text: str, record: dict, saved: bytes | None, expected: bytes) -> list[str]:
    match = DOWNLOAD.search(text)
    problems = []
    if not requests_of(record, "/file.bin"):
        problems.append("the server never saw /file.bin")
    if match is None:
        return problems + ["no DOWNLOAD:PASS with a name"]
    if int(match[2]) != len(expected):
        problems.append(f"the client counted {match[2]} bytes, the server sent {len(expected)}")
    if saved is None:
        problems.append(f"no {match[1]} in the person's Downloads folder on the OS volume")
    elif saved != expected:
        problems.append(f"the saved {match[1]} ({len(saved)} bytes) is not what the server sent")
    return problems


def latency(text: str) -> dict[str, int] | None:
    match = LATENCY.search(text)
    if match is None:
        return None
    keys = ["load_ms", "wheel_min", "wheel_median", "wheel_max", "wheel_n", "wheel_lost",
            "refresh_min", "refresh_median", "refresh_max"]
    return dict(zip(keys, (int(v) for v in match.groups())))


def judge_denies(text: str) -> list[str]:
    """No refused call of the client, webd or its children beyond the documented optional ones."""
    problems = []
    for line in sorted({m.group(0) for m in light.DENY.finditer(text) if OWN.fullmatch(m["label"])}):
        match = light.DENY.search(line)
        if "/" in match["label"] and any(re.fullmatch(rest, match["what"]) for rest in light.OPTIONAL_CHILD):
            continue
        problems.append(f"a refused call that is not a documented optional one: {line}")
    return problems[:16]


def window_region(text: str) -> tuple[int, int, int, int] | None:
    rects = [m for m in light.RECT.finditer(text) if m.group(5).strip() == f"window:{WINDOW}"]
    if not rects:
        return None
    return tuple(int(n) for n in rects[-1].groups()[:4])


def colours(path: Path, region: tuple[int, int, int, int]) -> dict[tuple[int, int, int], float]:
    """The share of each colour (quantised to 32 levels) in `region` of a screenshot."""
    width, height, channels, pixels = light.pngstats.decode_png(Path(path))
    x, y, w, h = region
    x0, y0, x1, y1 = max(x, 0), max(y, 0), min(x + w, width), min(y + h, height)
    if x1 <= x0 or y1 <= y0:
        raise ValueError(f"region {region} is outside the {width}x{height} screenshot")
    counts: dict[tuple[int, int, int], int] = {}
    for row in range(y0, y1, 2):
        base = row * width
        for col in range(x0, x1, 2):
            at = (base + col) * channels
            key = (pixels[at] >> 3, pixels[at + 1] >> 3, pixels[at + 2] >> 3)
            counts[key] = counts.get(key, 0) + 1
    total = sum(counts.values())
    return {key: count / total for key, count in counts.items()}


def judge_shot(path: Path | None, region, what: str) -> list[str]:
    """The window shows a page: at least two colours of the long page's bands, each over 5% of it."""
    if path is None or not Path(path).is_file():
        return [f"the {what} screenshot is missing"]
    if region is None:
        return [f"the window's rectangle is unknown (no UI:RECT name=window:{WINDOW}; UI probe?)"]
    try:
        shares = colours(Path(path), region)
    except (OSError, ValueError) as error:
        return [f"{what}: {error}"]
    big = [key for key, share in shares.items() if share > 0.05]
    if len(big) < 2:
        return [f"{what}: the window is one colour (a blank page?)"]
    return []
