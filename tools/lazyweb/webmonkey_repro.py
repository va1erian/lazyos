"""Reproduction bundles for the browser monkey (`webmonkey.py`): what a fixer needs, from the issue alone.

A finding is only useful if someone else can see it happen again. When the monkey meets one it writes, beside
`report.json`, a bundle per distinct finding (`repro_01.json`, `issue_01.md`):

* **where it was**: the page on show and the actions since the monkey last knew where it was, with the pause
  before each (idle time matters: a settled page must cost nothing). The replay starts by loading that page,
  so the 40 actions after it do not depend on a session that took ten minutes to build;
* **what to look for**: the finding's kind (`scroll-gap`, `wheel-answer`, `stuck-load`, `freeze`, ...), its
  measurement and the threshold it broke, so `--repro` can say REPRODUCED or NOT REPRODUCED by itself;
* **how it was built and run**: the commit, whether the tree was dirty, the display mode, memory, accelerator
  and the exact command, plus the serial lines around the finding.

`python tools/lazyweb/webmonkey.py --repro shots/webmonkey/run_001/repro_01.json` boots a fresh guest, runs the
steps and prints `WEBMONKEY: REPRODUCED <kind>` (exit 1) or `NOT REPRODUCED` (exit 0) after `--attempts` boots:
the same command, before and after a fix, is the test. The issue text is `issue_01.md`, ready to paste.
"""

from __future__ import annotations

import json
import re
import subprocess
from pathlib import Path

FORMAT = 1
#: Actions kept in a bundle: enough to rebuild the state, few enough to read.
TAIL = 40
#: Findings that need a bundle each, per run (the same kind on the same site is one bug).
MAX_BUNDLES = 6
#: A pause longer than this before an action is cut (an 8 s idle is the longest the monkey draws).
MAX_WAIT = 10.0

KINDS = [
    (re.compile(r"^jank: no frame was drawn"), "scroll-gap"),
    (re.compile(r"^jank: the page took"), "wheel-answer"),
    (re.compile(r"^jank: the window thread"), "window-frame"),
    (re.compile(r"^load stuck"), "stuck-load"),
    (re.compile(r"^freeze"), "freeze"),
    (re.compile(r"^(log: )?the page's process (stopped answering|was reported unresponsive)|^the window put up its hung-page bar"),
     "page-hang"),
    (re.compile(r"^(log: )?webd lost the window's view"), "view-lost"),
    (re.compile(r"^kernel fault"), "kernel-fault"),
    (re.compile(r"^a process was killed"), "process-killed"),
    (re.compile(r"waited \d+ ms for "), "lock-wait"),
    (re.compile(r"IPC message"), "ipc-lost"),
]
#: Findings of one kind that a hang or a lost view brings along: reproduced as the first.
SAME_BUG = {"page-hang": {"page-hang", "view-lost"}, "view-lost": {"page-hang", "view-lost"}}


def kind_of(finding: str) -> str:
    for pattern, kind in KINDS:
        if pattern.search(finding):
            return kind
    return "other"


def site_of(page: str) -> str:
    return re.sub(r"^(https?://[^/]+).*", r"\1", page) or "unknown"


def measurement(finding: str) -> float | None:
    m = re.search(r"(\d+(?:\.\d+)?) ms", finding)
    return float(m.group(1)) if m else None


def key_of(finding: str, page: str) -> tuple[str, str]:
    """One bug per kind (a hang and the view it loses are one) and site."""
    kind = kind_of(finding)
    return min(SAME_BUG.get(kind, {kind})), site_of(page)


def steps_from(trail: list[dict], start_page: str) -> dict:
    """The replay: load `page`, settle, then the last `TAIL` actions of `trail` ({act, page, wait} rows)."""
    kept = trail[-TAIL:]
    page = kept[0]["page"] if kept and kept[0]["page"].startswith("http") else start_page or "about:start"
    return {"page": page, "steps": [{"act": r["act"], "wait": min(round(r["wait"], 2), MAX_WAIT)} for r in kept]}


def bundle(finding: str, page: str, at: float, index: int, trail: list[dict], run: dict, excerpt: list[str]) -> dict:
    kind = kind_of(finding)
    return {
        "format": FORMAT, "kind": kind, "finding": finding, "site": site_of(page), "page": page,
        "measured_ms": measurement(finding), "found_at_s": round(at, 1), "found_at_action": index,
        "replay": steps_from(trail, run.get("start") or ""), "run": run, "serial_excerpt": excerpt,
    }


def run_facts(args, root: Path, seed: int) -> dict:
    """How the run was built and started, so the same guest can be made again."""
    def git(*a: str) -> str:
        done = subprocess.run(["git", *a], cwd=root, capture_output=True, text=True)
        return done.stdout.strip() if done.returncode == 0 else ""
    return {"commit": git("rev-parse", "HEAD"), "dirty": bool(git("status", "--porcelain", "--untracked-files=no")),
            "branch": git("rev-parse", "--abbrev-ref", "HEAD"), "seed": seed, "screen": args.screen,
            "memory": args.memory, "accel": args.accel, "start": args.start or "", "duration": args.duration,
            "jank_ms": args.jank_ms, "answer_ms": args.answer_ms,
            "build": "python tools/lazyweb/webmonkey.py --build  (an image for the real internet; LAZYOS_LAZYWEB=1)"}


def reproduced(want: dict, findings: list[str]) -> list[str]:
    """The findings of a replay that are the bundle's bug (same kind, or the same hang family)."""
    kinds = SAME_BUG.get(want["kind"], {want["kind"]})
    return [f for f in findings if kind_of(f) in kinds]


def render_issue(b: dict, name: str, out: Path) -> str:
    run, steps = b["run"], b["replay"]["steps"]
    lines = [
        f"# LazyWeb: {b['kind']} on {b['site']}",
        "",
        f"The browser monkey found: `{b['finding']}`",
        f"after {b['found_at_s']:.0f} s and {b['found_at_action']} actions (seed {run['seed']}).",
        "",
        "## Reproduce",
        "",
        f"Commit `{run['commit'][:12]}`" + (" (the tree had uncommitted changes)" if run["dirty"] else "")
        + f", branch `{run['branch']}`, display {run['screen']}, {run['memory']} RAM, accelerator {run['accel']}.",
        "The image needs the real internet through QEMU's user network. Build it once, then replay:",
        "",
        "```bash",
        "python tools/lazyweb/webmonkey.py --build --duration 1   # builds target/lazyos.img (add --start for a page)",
        f"python tools/lazyweb/webmonkey.py --repro {out.as_posix()}/{name}.json --attempts 3",
        "```",
        "",
        f"If the short replay does not show it (some bugs need minutes on a page), the whole run is: `python "
        f"tools/lazyweb/webmonkey.py --seed {run['seed']} --duration {run['duration']:g}"
        + (f" --start {run['start']}" if run["start"] else "") + " --keep-going` (guest timing varies, so a seed is not a promise).",
        "",
        f"`--repro` boots a fresh guest, loads `{b['replay']['page']}`, waits for it, and sends the {len(steps)} "
        "recorded actions with their pauses. It prints `WEBMONKEY: REPRODUCED " + b["kind"] + "` (exit 1) when the same "
        "kind of finding appears. A live page changes, so a few attempts may be needed.",
        "",
        "## Actions (after loading the page)",
        "",
        "```",
        *[f"+{s['wait']:>5}s  {json.dumps(s['act'])}" for s in steps],
        "```",
        "",
        "## What to read",
        "",
        "* `LAZYWEB:INFO:SMOOTH` (client frame meter: `gapmax` in us, only while `wheel` > 0), "
        "`WEBD:NOTE:INFO:PAINT` (`w2d=<avg>/<worst>` in us), `PERF:CPU`, `SCHED:GUARD:SUMMARY`, `LOCK:WAIT`.",
        "* Serial excerpt around the finding:",
        "",
        "```",
        *b["serial_excerpt"][-40:],
        "```",
        "",
        f"Artifacts of the run (screenshot, full serial log, `report.json`): `{out.as_posix()}`.",
    ]
    return "\n".join(lines) + "\n"


def write(b: dict, number: int, out: Path) -> None:
    name = f"repro_{number:02d}"
    (out / f"{name}.json").write_text(json.dumps(b, indent=1), encoding="utf-8")
    (out / f"issue_{number:02d}.md").write_text(render_issue(b, name, out), encoding="utf-8")


def load(path: Path) -> dict:
    data = json.loads(path.read_text(encoding="utf-8"))
    if data.get("format") != FORMAT:
        raise SystemExit(f"{path}: repro format {data.get('format')!r}, this tool reads {FORMAT}")
    return data
