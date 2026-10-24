"""What the browser monkey watches in the serial log (`webmonkey.py`): pure functions, no I/O.

`Watch.feed(lines)` reads new serial lines as they arrive and keeps the state a person would read
off the screen: the page on show, whether a load is in flight and for how long, how often the window
thread overran a frame. It returns findings (a freeze, a lost view, a stuck load, a frame that took
too long, a crash) as strings; the monkey stops or notes them. A final pass over the whole log with
`live_run.verdict` adds the kernel-side signs of a hang (lock waits, stuck calls, crashed processes).
"""

from __future__ import annotations

import re
from dataclasses import dataclass, field

P = "LAZYWEB"
NOTE = "WEBD:NOTE:"

#: Lines that are a finding whatever the monkey was doing.
FATAL = [
    (re.compile(r"EXCEPTION:|LazyOS PANIC|double fault"), "kernel fault"),
    (re.compile(r"killed by |oom: killed "), "a process was killed"),
    (re.compile(rf"{P}:(BIND|RUN):FAIL"), "the browser failed to start"),
    (re.compile(r"WEBD:LOST view="), "webd lost the window's view"),
    (re.compile(rf"{NOTE}WEBPROCESS:UNRESPONSIVE"), "the page's process stopped answering"),
    (re.compile(rf"{P}:HANG:SHOW"), "the window put up its hung-page bar"),
    (re.compile(r"Error sending IPC message|PROC:SCM:REFUSE"), "WebKit lost an IPC message"),
    (re.compile(r"SCHED:STUCK"), "a task was stuck in a call"),
]
#: Page-load errors are the web being the web (dead links, refused connections): counted, not findings.
LOAD_FAIL = re.compile(rf"{P}:FAIL:")
#: What a scroll feels like, once a second while the wheel turns: the longest gap between drawn frames
#: (`gapmax`, us), gaps over 33 ms, and the share of rows shown as checkerboard (`INFO:SMOOTH`, the client's).
SMOOTH = re.compile(rf"{P}:INFO:SMOOTH:(\S+)")
#: The engine's meter, relayed by webd: the wheel's answer, `w2d=<average>/<worst>` in us.
PAINT_W2D = re.compile(rf"{NOTE}INFO:PAINT:.*?:wheel=(\d+):.*?:w2d=(\d+)/(\d+)")
WINDOW = re.compile(rf"{P}:INFO:WINDOW:.*?max=(\d+)")
LOAD = re.compile(rf"{P}:LOAD:(\S+)")
#: A new page took the view (it may load for seconds more): from here the frames and the wheel are its own.
COMMIT = re.compile(rf"{P}:COMMIT:(\S+)")
NAV = re.compile(rf"{P}:NAV:(\S+)")
TITLE = re.compile(rf"{P}:TITLE:(.*)")
STOPPED = re.compile(rf"{P}:STOPPED")
#: Lines that echo what a page chose (its title: `TITLE:`, the probe's `window=<title> - LazyWeb`; its address,
#: which a script sets with pushState: `NAV`, `COMMIT`, `LOAD`, `POPUP`) say nothing about the system.
ECHOES_TITLE = re.compile(rf"^(UI:|{P}:(TITLE|NAV|COMMIT|LOAD|POPUP):)")
QUIT = re.compile(rf"{P}:QUIT:")
LOCK_WAIT = re.compile(r"LOCK:WAIT lock=(\w+) holder=(\S+) .*? ms=(\d+) comm=(\S+)")


@dataclass
class Watch:
    """The browser's state from its markers, and the findings so far."""

    jank_ms: float = 100.0  # a window-thread frame longer than this is jank
    lock_ms: int = 500
    answer_ms: float = 400.0  # a wheel answer slower than this is jank (the engine's own meter)
    page: str = ""  # the newest address named: a navigation in flight, or the page it ended on
    on_screen: str = ""  # the page that committed last (`COMMIT`, `LOAD`): what the wheel was turning and the frames drew
    title: str = ""
    loading_since: float | None = None
    findings: list[str] = field(default_factory=list)
    load_fails: int = 0
    loads: int = 0
    worst_frame_ms: float = 0.0  # the window thread's costliest frame: its own work, not what a person sees
    worst_gap_ms: float = 0.0  # the longest wait between two drawn frames while the wheel turned
    worst_answer_ms: float = 0.0  # the slowest wheel-to-damage answer from the page
    scroll_seconds: int = 0
    checker_seconds: int = 0
    janks: list[tuple[float, float]] = field(default_factory=list)  # (when, ms)
    quit: bool = False

    def feed(self, lines: list[str], now: float) -> list[str]:
        new: list[str] = []
        for line in lines:
            new += self._one(line, now)
        self.findings += new
        return new

    def _one(self, line: str, now: float) -> list[str]:
        found: list[str] = []
        if m := NAV.search(line):
            self.page, self.loading_since = m.group(1), self.loading_since or now
        elif m := COMMIT.search(line):
            self.on_screen = m.group(1)
        elif m := LOAD.search(line):
            self.page = self.on_screen = m.group(1)
            self.loading_since, self.loads = None, self.loads + 1
        elif m := TITLE.search(line):
            self.title = m.group(1).strip()
        elif LOAD_FAIL.search(line):
            self.loading_since, self.load_fails = None, self.load_fails + 1
        elif STOPPED.search(line):
            self.loading_since = None
        elif QUIT.search(line):
            self.quit = True
        elif m := SMOOTH.search(line):
            found += self._smooth(dict(p.partition("=")[::2] for p in m.group(1).split(":")), now)
        elif m := PAINT_W2D.search(line):
            worst = int(m.group(3)) / 1000
            if int(m.group(1)) > 0:
                self.worst_answer_ms = max(self.worst_answer_ms, worst)
                if worst >= self.answer_ms:
                    self.janks.append((now, worst))
                    found.append(f"jank: the page took {worst:.0f} ms to answer the wheel (page {self.on_screen or self.page})")
        elif m := WINDOW.search(line):
            ms = int(m.group(1)) / 1000
            self.worst_frame_ms = max(self.worst_frame_ms, ms)
            if ms >= self.jank_ms:
                self.janks.append((now, ms))
                found.append(f"jank: the window thread spent {ms:.0f} ms on one frame (page {self.on_screen or self.page})")
        elif (m := LOCK_WAIT.search(line)) and int(m.group(3)) >= self.lock_ms:
            found.append(f"{m.group(4)} waited {m.group(3)} ms for {m.group(1)}, held by {m.group(2)}")
        if ECHOES_TITLE.match(line):
            return found
        for pattern, what in FATAL:
            if pattern.search(line):
                found.append(f"{what}: {line.strip()[:200]}")
                break
        return found

    def _smooth(self, row: dict, now: float) -> list[str]:
        """One second of the client's frame meter; only seconds with wheel input are scrolling."""
        if int(row.get("wheel", "0") or 0) <= 0:
            return []
        self.scroll_seconds += 1
        gap = int(row.get("gapmax", "0") or 0) / 1000
        self.worst_gap_ms = max(self.worst_gap_ms, gap)
        shown = int(row.get("shown", "0") or 0)
        if shown and int(row.get("checker", "0") or 0) * 4 > shown:  # over a quarter of the rows were blank
            self.checker_seconds += 1
        if gap >= self.jank_ms:
            self.janks.append((now, gap))
            return [f"jank: no frame was drawn for {gap:.0f} ms while scrolling (page {self.on_screen or self.page})"]
        return []

    def stuck_load(self, now: float, limit: float) -> str | None:
        """A load that began over `limit` seconds ago and has neither finished nor failed."""
        if self.loading_since is not None and now - self.loading_since > limit:
            return f"load stuck for {now - self.loading_since:.0f} s: {self.page}"
        return None

    def reset_load(self) -> None:
        """Forget the load in flight (the monkey stopped it, or reported it)."""
        self.loading_since = None


def summary(watch: Watch) -> dict:
    return {"loads": watch.loads, "load_fails": watch.load_fails, "worst_window_frame_ms": round(watch.worst_frame_ms, 1),
            "worst_scroll_gap_ms": round(watch.worst_gap_ms, 1), "worst_wheel_answer_ms": round(watch.worst_answer_ms, 1),
            "scroll_seconds": watch.scroll_seconds, "checkerboard_seconds": watch.checker_seconds,
            "janks": len(watch.janks), "findings": len(watch.findings), "last_page": watch.page}
