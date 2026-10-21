"""The verdict of `run.py` (docs/webd-plan.md, P2 slice 2): pure functions over the guest's serial
log, the requests the host's listener saw and the screenshots, each returning a list of problems
(empty: it passed). `test_judge.py` feeds them canned logs and checks they fail when they should.

The markers are the program's: `WEBD:*` from `webd` (`xui-app/crates/webd` notes, `webkit/port/app`
engine lines), `WEBDDEMO:*` from the demo client (`xui_webd_client::demo::Marker::text`,
`xui-app/src/bin/webddemo.rs`), `UI:RECT` from `xuid`. The page and its constants are
`xui-app/crates/webd-client/src/page.rs`; `test_judge.py` reads that file and fails when these
copies go stale.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

sys.path.append(str(Path(__file__).resolve().parent.parent / "screenshot"))
import pngstats  # noqa: E402

D = "WEBDDEMO"
W = "WEBD"
#: The demo window's title and the page's link (`page.rs`).
WINDOW = "Web View Demo"
LINK_URL = "https://example.com/clicked"
#: The listener's port: the demo page's picture is `http://10.0.2.2:18080/blocked.png`.
BLOCKED_PORT = 18080
BLOCKED_PATH = "/blocked.png"
#: The first page may wait for the engine to start its processes; past this the light view is too slow
#: to be a light view (a TCG guest takes seconds, not a minute).
LATENCY_MAX_MS = 60_000
#: A shot of the demo window counts as showing the page when this share of its pixels is the page's blue.
BLUE_SHARE = 0.5

#: The reasons `WEBDDEMO:LOST:` may give for a killed web process.
KILLED_REASONS = ("crashed",)

# `WEBD:NOTE:<engine line>` relays the engine's own markers, among them the `FAIL:webprocess:crashed` that the
# loss test provokes on purpose: a NOTE is not a failure of webd.
FAIL_LINE = re.compile(rf"^(?:{D}|{W})(?::(?!NOTE:)[A-Z_]+)*:FAIL[^\r\n]*", re.M)
VIEW = re.compile(rf"^{D}:VIEW:PASS id=(\d+)", re.M)
FRAME = re.compile(rf"^{D}:FRAME:PASS colour=(\w+) w=(\d+) h=(\d+) seq=(\d+) ms=(\d+)", re.M)
LATENCY = re.compile(rf"^{D}:LATENCY:(\d+)", re.M)
LINK = re.compile(rf"^{D}:LINK:(\S+)", re.M)
STATE = re.compile(rf"^{D}:STATE:([^\r\n]*)", re.M)
LOST = re.compile(rf"^{D}:LOST:(\w+)", re.M)
RECREATE = re.compile(rf"^{D}:RECREATE:PASS", re.M)
QUIT = re.compile(rf"^{D}:QUIT:PASS", re.M)
CREATE = re.compile(rf"^{W}:VIEW:CREATE client=(\S+) view=(\d+)", re.M)
DESTROY = re.compile(rf"^{W}:VIEW:DESTROY view=(\d+)", re.M)
WEBD_LOST = re.compile(rf"^{W}:LOST view=(\d+) reason=(\S+)", re.M)
RECT = re.compile(r"UI:RECT x=(-?\d+) y=(-?\d+) w=(\d+) h=(\d+) name=([^\r\n]*)")


def at(pattern: re.Pattern, text: str, nth: int = 0) -> int | None:
    """The offset of the `nth` match of `pattern`, or None."""
    found = [m.start() for m in pattern.finditer(text)]
    return found[nth] if nth < len(found) else None


def judge_failures(text: str) -> list[str]:
    """No marker of webd or the demo says FAIL (`WEBD:...:FAIL`, `WEBDDEMO:CREATE:FAIL:...`)."""
    return [f"the guest reported a failure: {m.group(0)}" for m in FAIL_LINE.finditer(text)][:8]


def judge_webd(text: str) -> list[str]:
    """webd came up and registered the light service."""
    problems = []
    if not re.search(rf"^{W}:UP:PASS", text, re.M):
        problems.append("webd never printed WEBD:UP:PASS")
    if f"{W}:SERVE:app.os.lazy.webd.light" not in text:
        problems.append("webd never printed WEBD:SERVE:app.os.lazy.webd.light")
    return problems


def judge_views(text: str) -> list[str]:
    """Every view the demo says it has, webd created for the demo's own label."""
    problems = []
    views = [int(n) for n in VIEW.findall(text)]
    if not views:
        return ["the demo never printed WEBDDEMO:VIEW:PASS"]
    created = {int(view): client for client, view in CREATE.findall(text)}
    for view in views:
        client = created.get(view)
        if client is None:
            problems.append(f"the demo's view {view} has no WEBD:VIEW:CREATE line")
        elif client == "none" or (client.startswith("app:") and "webddemo" not in client):
            # webd cannot name another app's label (the credential gate shows a label's name only to
            # its own holder or to CAP_SETUID), so a client is `label#<id>` there: any such id passes.
            problems.append(f"view {view} was created for {client}, not for the demo")
    if len(set(views)) != len(views):
        problems.append(f"a view id was handed out twice: {views}")
    return problems


def judge_page(text: str, latency_max_ms: int = LATENCY_MAX_MS) -> list[str]:
    """The first page showed: a blue frame after the view, a latency that agrees with it, the title."""
    problems = []
    frames = FRAME.findall(text)
    if not frames:
        return ["the demo never printed WEBDDEMO:FRAME:PASS (no non-blank frame of the page)"]
    colour, width, height, seq, ms = frames[0]
    if colour != "blue":
        problems.append(f"the first frame of the page is {colour}, not blue")
    if int(width) < 64 or int(height) < 64:
        problems.append(f"the first frame is {width}x{height}")
    if int(seq) < 1:
        problems.append(f"frame number {seq}")
    latencies = [int(n) for n in LATENCY.findall(text)]
    if not latencies:
        problems.append("the demo never printed WEBDDEMO:LATENCY")
    elif latencies[0] != int(ms):
        problems.append(f"LATENCY {latencies[0]} ms does not agree with the frame's ms={ms}")
    elif latencies[0] > latency_max_ms:
        problems.append(f"the first page took {latencies[0]} ms (more than {latency_max_ms})")
    if WINDOW not in STATE.findall(text):
        problems.append(f"no WEBDDEMO:STATE:{WINDOW} (the page's title)")
    view_at, frame_at = at(VIEW, text), at(FRAME, text)
    if view_at is not None and frame_at is not None and frame_at < view_at:
        problems.append("a frame was reported before the view")
    return problems


def judge_link(text: str, expected: bool = True) -> list[str]:
    """A click on the link raised LinkClicked with its address, and nothing else navigated."""
    links = LINK.findall(text)
    problems = []
    if expected and LINK_URL not in links:
        problems.append(f"no WEBDDEMO:LINK:{LINK_URL} after the click")
    # The form's GET adds a query; anything else is a link the page should not have raised.
    problems += [f"unexpected link {link}" for link in links if not link.startswith(LINK_URL)]
    return problems


def judge_requests(record: dict) -> list[str]:
    """A light view has no network: the picture of the page, or anything else, never reached the host.
    `record`: the listener's `{"requests": [...], "connections": n}`."""
    requests = record.get("requests", [])
    problems = [f"the listener on port {BLOCKED_PORT} saw {r.get('method', '?')} {r.get('path', '?')} "
                f"from {r.get('client', '?')}: a light view loaded something" for r in requests[:8]]
    if record.get("connections", 0) > len(requests):
        problems.append(f"{record['connections']} connection(s) reached port {BLOCKED_PORT} but only "
                        f"{len(requests)} request(s): something connected and sent nothing")
    return problems


def judge_loss(text: str, expected: bool) -> list[str]:
    """The web process was killed: the demo said LOST, made a new view once, and got its page back."""
    lost, recreated = LOST.findall(text), RECREATE.search(text)
    if not expected:
        return [f"the view was lost without the session asking: {lost}"] if lost else []
    problems = []
    if not lost:
        return ["the demo never printed WEBDDEMO:LOST after the web process was killed (did the kill land?)"]
    if lost[0] not in KILLED_REASONS:
        problems.append(f"WEBDDEMO:LOST:{lost[0]}, expected one of {KILLED_REASONS}")
    views = [int(n) for n in VIEW.findall(text)]
    lost_at, recreate_at = at(LOST, text), at(RECREATE, text)
    if recreated is None:
        problems.append("the demo never printed WEBDDEMO:RECREATE:PASS")
    elif lost_at is not None and recreated.start() < lost_at:
        problems.append("RECREATE:PASS came before LOST")
    if len(views) < 2 or views[1] == views[0]:
        problems.append(f"no second, different view after the loss: {views}")
    elif recreate_at is not None:
        second = at(VIEW, text, 1)
        if second is not None and second > recreate_at:
            problems.append("the new view was announced after RECREATE:PASS")
    if views:
        gone = {int(view): reason for view, reason in WEBD_LOST.findall(text)}
        if views[0] not in gone:
            problems.append(f"webd never printed WEBD:LOST view={views[0]}")
        destroyed = {int(n) for n in DESTROY.findall(text)}
        if views[0] not in destroyed:
            problems.append(f"the lost view {views[0]} was never destroyed at webd (WEBD:VIEW:DESTROY)")
    return problems


def judge_quit(text: str) -> list[str]:
    """The demo quit, and webd then held no view of it."""
    problems = []
    if not QUIT.search(text):
        problems.append("the demo never printed WEBDDEMO:QUIT:PASS")
    created = [int(view) for _, view in CREATE.findall(text)]
    destroyed = {int(n) for n in DESTROY.findall(text)}
    left = [view for view in created if view not in destroyed]
    if left:
        problems.append(f"views never destroyed at webd after the demo quit: {left}")
    return problems


#: What a web process may be refused without the page breaking: the engine looks up a few optional
#: things and falls back (carried over from LazyWeb+'s documented set, `tools/webplus/denies.py`,
#: because the engine and the process confinement are the same; a run of this harness with
#: `--label-trace` is what prunes it). The demo's own label has none.
OPTIONAL_CHILD = [
    r"file=/etc access=stat",
    r"file=/etc/localtime access=stat",
    r"file=/usr/share/zoneinfo access=stat",
    r"file=/\S*/fontconfig/conf\.avail access=read",
    r"file=/system/share/fonts(/\S+)? access=write",
    r"file=/tmp(/fontconfig)? access=write",
    r"file=/etc/ssl/openssl\.cnf access=stat",  # the network process
]
DENY = re.compile(r"LABEL:DENY label=(?P<label>\S+) (?P<what>.*?)\s*$", re.M)
#: The labels this harness answers for: the demo, webd and webd's children.
OWN = re.compile(r"app:os\.lazy\.webd(demo)?(/\S+)?$")


def denies(text: str) -> list[str]:
    """The distinct `LABEL:DENY` lines of the demo's, webd's and webd's children's labels."""
    return sorted({m.group(0) for m in DENY.finditer(text) if OWN.fullmatch(m["label"])})


def judge_denies(text: str) -> list[str]:
    """No refused call of the demo or webd beyond the documented optional set of webd's children."""
    problems = []
    for line in denies(text):
        match = DENY.search(line)
        child = "/" in match["label"]
        if child and any(re.fullmatch(rest, match["what"]) for rest in OPTIONAL_CHILD):
            continue
        problems.append(f"a refused call that is not a documented optional one: {line}")
    return problems[:16]


def window_region(text: str) -> tuple[int, int, int, int] | None:
    """The demo window's content on the screen, from the newest `UI:RECT ... name=window:Web View Demo`."""
    rects = [m for m in RECT.finditer(text) if m.group(5).strip() == f"window:{WINDOW}"]
    if not rects:
        return None
    x, y, w, h = (int(n) for n in rects[-1].groups()[:4])
    return x, y, w, h


def blue_share(path: Path, region: tuple[int, int, int, int]) -> float:
    """The share of the pixels of `region` of a screenshot that are the page's blue."""
    width, height, channels, pixels = pngstats.decode_png(Path(path))
    x, y, w, h = region
    x0, y0, x1, y1 = max(x, 0), max(y, 0), min(x + w, width), min(y + h, height)
    if x1 <= x0 or y1 <= y0:
        raise ValueError(f"region {region} is outside the {width}x{height} screenshot")
    blue = total = 0
    for row in range(y0, y1):
        base = row * width
        for col in range(x0, x1):
            at_ = (base + col) * channels
            r, g, b = pixels[at_], pixels[at_ + 1], pixels[at_ + 2]
            blue += r < 80 and g < 80 and b > 200
            total += 1
    return blue / total


def judge_shot(path: Path | None, region: tuple[int, int, int, int] | None, what: str) -> list[str]:
    """The demo window of one screenshot shows the page's colour."""
    if path is None or not Path(path).is_file():
        return [f"the {what} screenshot is missing"]
    if region is None:
        return [f"the demo window's rectangle is unknown (no UI:RECT name=window:{WINDOW}); is the image "
                "built with LAZYOS_UI_PROBE=1?"]
    try:
        share = blue_share(Path(path), region)
    except (OSError, ValueError) as error:
        return [f"{what}: {error}"]
    if share < BLUE_SHARE:
        return [f"{what}: only {share:.0%} of the demo window is the page's blue (need {BLUE_SHARE:.0%})"]
    return []
