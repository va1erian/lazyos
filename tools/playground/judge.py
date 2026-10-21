"""What the Messenger Playground's guest sessions must show on serial and on screen.

Pure functions over the serial log text (`PLAYGROUND:*` markers, the `tool:` label install and
launch lines `pkgd` and `init` print, `LABEL:DENY` lines, `PROC:EXIT` lines) and over the
screenshots, each returning a list of problems. `run.py` calls them; `test_judge.py` proves they
fail when they should. Markers never carry a script, an argument, a payload or a reply
(docs/messenger-playground-plan.md section 4), so nothing here reads any.
"""

from __future__ import annotations

import json
import re
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
sys.path.insert(0, str(ROOT / "tools"))
sys.path.insert(0, str(ROOT / "tools" / "screenshot"))
import serial_integrity  # noqa: E402  (a log with SERIAL:DROPPED proves nothing)

P = "PLAYGROUND"
APP = "os.lazy.playground"
LABEL = f"tool:{APP}"
TITLE = "Messenger Playground (developer tool)"
#: The Topic monitor, the app's second window.
TOPICS_TITLE = "Messenger Topics (developer tool)"
#: The screen the session boots at (`qemu_qmp.py`'s default).
SCREEN = (1280, 720)
#: The interfaces a script of the sessions may be refused (the developer-tool deny list).
EXPECTED_DENIED = {"os.lazy.keyd.v1"}


def lines(text: str, prefix: str) -> list[str]:
    """The lines that start with `prefix` (the serial log has one marker per line)."""
    return [line.strip() for line in text.splitlines() if line.startswith(prefix)]


def markers(text: str) -> list[str]:
    """Every `PLAYGROUND:` line, without the prefix."""
    return [line[len(P) + 1:] for line in lines(text, P + ":")]


def runs(text: str) -> dict[int, dict]:
    """`RUN:<id>:START:<hash>` and `RUN:<id>:EXIT:<status>:<hash>` by run id."""
    out: dict[int, dict] = {}
    for m in markers(text):
        found = re.fullmatch(r"RUN:(\d+):(START|EXIT):(?:([^:]+):)?([0-9a-f]{16})", m)
        if not found:
            continue
        entry = out.setdefault(int(found.group(1)), {"hash": found.group(4)})
        if found.group(2) == "START":
            entry["start"] = True
        else:
            entry["exit"] = found.group(3)
    return out


def judge_integrity(text: str) -> list[str]:
    return serial_integrity.problems(text)


def judge_install(text: str, first_boot: bool = False) -> list[str]:
    """First real exercise of the `tool:` label: pkgd installs it, init starts under it.

    A later boot of the same image finds the package already installed, so only the first boot
    prints the `INSTALL` line.
    """
    problems = []
    need = [
        (rf"PKGD:POLICY:PASS {re.escape(LABEL)} rules=\d+", "pkgd did not load the tool: label's rules"),
        (rf"PKGD:PROVISION:INSTALL {re.escape(APP)} " if first_boot else r"PKGD:PROVISION:DONE",
         "pkgd did not install the package"),
        (rf"INIT:LAUNCH:PASS app={re.escape(APP)} ", "init did not launch the app"),
        (rf"PKGD:LAUNCH:LABEL {re.escape(LABEL)} pid=\d+", "the app did not start under its tool: label"),
    ]
    for pattern, why in need:
        if not re.search(pattern, text):
            problems.append(why)
    return problems


def judge_app(text: str) -> list[str]:
    """The window: title, strip, the evidence a developer tool prints, a clean exit."""
    problems = []
    if f"{P}:UP:PASS" not in text:
        problems.append(f"{P}:UP:PASS is missing")
    if f"{P}:DEVTOOL:UP" not in text:
        problems.append(f"{P}:DEVTOOL:UP is missing (the app did not start as a developer tool)")
    if not re.search(rf"UI:WIDGET .* name=devtool:strip window={re.escape(TITLE)}", text):
        problems.append("the developer-tool strip was never probed (not on screen)")
    if f"name=window:{TITLE}" not in text:
        problems.append(f"no window titled '{TITLE}'")
    if f"{P}:QUIT:PASS" not in text:
        problems.append(f"{P}:QUIT:PASS is missing")
    if not re.search(rf"INIT:LAUNCH:EXIT app={re.escape(APP)} status=0", text):
        problems.append("the app did not exit with status 0")
    for m in markers(text):
        if re.search(r":(FAIL|BIND:FAIL)\b", m) or m.startswith("RUN:FAIL"):
            problems.append(f"{P}:{m}")
    if re.search(r"LazyOS PANIC|EXCEPTION:", text):
        problems.append("a kernel panic or exception")
    return problems


def judge_monitor(text: str) -> list[str]:
    """The Topic monitor, when a session opened it: it came up (with its strip), and it went
    before the main window said QUIT (closing the tool closes it, which ends its subscriptions)."""
    up, quit_ = f"{P}:TOPICS:UP", f"{P}:TOPICS:QUIT"
    if up not in text:
        return [f"{quit_} without {up}"] if quit_ in text else []
    problems = []
    if quit_ not in text:
        problems.append(f"{quit_} is missing (the Topic monitor was left open)")
    elif text.index(quit_) > text.index(f"{P}:QUIT:PASS"):
        problems.append("the Topic monitor closed after the main window quit")
    if f"name=window:{TOPICS_TITLE}" not in text:
        problems.append(f"no window titled '{TOPICS_TITLE}'")
    if not re.search(rf"UI:WIDGET .* name=devtool:strip window={re.escape(TOPICS_TITLE)}", text):
        problems.append("the Topic monitor's developer-tool strip was never probed")
    return problems


def judge_orphans(text: str) -> list[str]:
    """Every `rhai` the runs started is gone by the end (closing leaves none behind)."""
    started = sum(1 for r in runs(text).values() if r.get("start") and r.get("exit") != "failed")
    exited = 0
    for line in lines(text, "PROC:EXIT"):
        if "comm=rhai" in line:
            exited += 1
    if exited < started:
        return [f"{started} runs started a child but only {exited} rhai processes exited"]
    return []


def interface_names(manifest: Path | None = None) -> dict[int, str]:
    """`idl/manifest.json` as interface id -> name."""
    path = manifest or ROOT / "idl" / "manifest.json"
    return {int(item["interface_id"], 16): item["interface"] for item in json.loads(path.read_text())}


def denials(text: str) -> list[tuple[str, str]]:
    """`(label, what)` of every `LABEL:DENY` line, `what` being `iface=..` etc."""
    out = []
    for line in lines(text, "LABEL:DENY"):
        label = re.search(r"label=(\S+)", line)
        rest = " ".join(w for w in line.split()[1:] if not w.startswith("label="))
        out.append((label.group(1) if label else "?", rest))
    return out


def judge_denials(text: str, names: dict[int, str], allowed: set[str] = frozenset()) -> list[str]:
    """No refused call of the app's own: only the deny-listed interfaces a session's script calls."""
    problems = []
    for label, what in denials(text):
        if not label.startswith(LABEL):
            continue
        found = re.search(r"iface=(\S+)", what)
        name = names.get(int(found.group(1), 0)) if found else None
        if name in allowed:
            continue
        problems.append(f"unexpected LABEL:DENY for the app: {what}")
    return problems


def _need(text: str, pattern: str, why: str) -> list[str]:
    return [] if re.search(pattern, text, re.M) else [why]


def judge_browse(text: str) -> list[str]:
    return _need(text, rf"^{P}:OPEN:os\.lazy\.confd\.v1$", "opening confd's method did not print OPEN")


def judge_run(text: str) -> list[str]:
    """Run 1 a script, 2 the Call form (no arguments), 3 the Call form (a path), 4 a refused
    call, 5 a script that waits until Stop."""
    problems = []
    seen = runs(text)
    if seen.get(1, {}).get("exit") != "0":
        problems.append("run 1 (a script printing one line) did not exit 0")
    problems += _need(text, rf"^{P}:CALL:os\.lazy\.confd\.v1\.Info:OK$", "the Call form's confd.Info call did not report OK")
    problems += _need(text, rf"^{P}:CALL:os\.lazy\.confd\.v1\.Get:OK$", "the Call form's confd.Get call (with a path) did not report OK")
    if seen.get(4, {}).get("exit") in (None, "0", "stopped"):
        problems.append("run 4 (the keyd call) did not fail: a refused call must not succeed")
    if not seen.get(5, {}).get("start"):
        problems.append("run 5 (msg::run) never started")
    if seen.get(5, {}).get("exit") != "stopped":
        problems.append("Stop did not end run 5 (exit status 'stopped')")
    return problems


def judge_serve(text: str) -> list[str]:
    """Run 1 serves, run 2 calls it, Stop ends run 1; run 3 serves again and is live at Quit."""
    problems = []
    seen = runs(text)
    if not seen.get(1, {}).get("start"):
        problems.append("run 1 (serving demo.pirate) never started")
    if seen.get(2, {}).get("exit") != "0":
        problems.append("run 2 (calling demo.pirate) did not exit 0: the served name was not answered")
    if seen.get(1, {}).get("exit") != "stopped":
        problems.append("Stop did not end the serving run")
    if not seen.get(3, {}).get("start"):
        problems.append("run 3 (serving again, left live at Quit) never started")
    elif seen[3].get("exit") not in (None, "stopped"):
        problems.append("run 3 ended on its own: it was meant to be live when the window closed")
    return problems


def judge_listen(text: str) -> list[str]:
    """Peek (run 1) in the main window, then Listen opens the Topic monitor: the three topics
    are listened to and decoded there, and its Publish form has the main window run run 2."""
    problems = []
    if f"{P}:TOPICS:UP" not in text:
        problems.append("Listen never opened the Topic monitor")
    elif not re.search(rf"UI:WIDGET .* name=pub_go window={re.escape(TOPICS_TITLE)}", text):
        problems.append("the Topic monitor's controls were never probed (not on screen)")
    seen = runs(text)
    for run_id, what in ((1, "Peek"), (2, "Publish")):
        if seen.get(run_id, {}).get("exit") != "0":
            problems.append(f"run {run_id} ({what}) did not exit 0")
    for topic in ("time/tick", "system/stats/memory", "demo/pg/x"):
        problems += _need(text, rf"^{P}:LISTEN:{re.escape(topic)}$", f"never listened to {topic}")
        problems += _need(text, rf"^{P}:EVENT:{re.escape(topic)}:\d+$", f"no event arrived on {topic}")
    return problems


def judge_elevd(text: str) -> list[str]:
    """A tool's script asks `elevd` for a restart: the trusted prompt appears and the grant lands."""
    problems = _need(text, r"XUID:PROMPT:UP", "the trusted prompt never came up")
    problems += _need(text, r"ELEVD:REQUEST op=service\.restart .*outcome=granted",
                      "elevd did not grant the restart after the administrator approved")
    if runs(text).get(1, {}).get("exit") != "0":
        problems.append("the asking run did not exit 0")
    return problems


def judge_notice(text: str) -> list[str]:
    """The strip's "x" was probed and pressed: the band is hidden and said so."""
    problems = _need(text, rf"UI:WIDGET .* name=devtool:strip_dismiss window={re.escape(TITLE)}",
                     "the strip's dismiss button was never probed")
    problems += _need(text, rf"^{P}:NOTICE:HIDDEN$", "dismissing the notice printed no NOTICE:HIDDEN")
    return problems


def judge_notice_stored(text: str) -> list[str]:
    """A later launch starts with the notice hidden by the stored choice, then View > Show
    brings it back: stored, then shown, and nothing dismissed it again."""
    problems = _need(text, rf"^{P}:NOTICE:HIDDEN:stored$", "the stored dismissal was not honoured at start")
    problems += _need(text, rf"^{P}:NOTICE:SHOWN$", "Show developer-tool notice printed no NOTICE:SHOWN")
    if re.search(rf"^{P}:NOTICE:HIDDEN$", text, re.M):
        problems.append("the notice was dismissed again in the session that restores it")
    if text.find(f"{P}:NOTICE:SHOWN") < text.find(f"{P}:NOTICE:HIDDEN:stored"):
        problems.append("NOTICE:SHOWN came before the stored dismissal")
    return problems


def widget_rects(text: str, name: str, window: str = TITLE) -> list[tuple[int, int, int, int]]:
    """Every `UI:WIDGET` rectangle printed for `name` in `window`, oldest first (x, y, w, h)."""
    out = []
    for line in lines(text, "UI:WIDGET"):
        found = re.fullmatch(
            rf"UI:WIDGET x=(-?\d+) y=(-?\d+) w=(\d+) h=(\d+) name={re.escape(name)} window={re.escape(window)}", line)
        if found:
            out.append(tuple(int(found.group(i)) for i in range(1, 5)))
    return out


def layout_events(text: str) -> list[str]:
    """`LAYOUT:BOTTOM:SHOWN|HIDDEN` and `RUN:<id>:START` in the order the app printed them."""
    out = []
    for m in markers(text):
        if m.startswith("LAYOUT:BOTTOM:"):
            out.append(m.rsplit(":", 1)[1])
        elif re.match(r"RUN:\d+:START:", m):
            out.append("RUN")
    return out


def judge_layout(text: str) -> list[str]:
    """Ctrl+J, the button, two dragged dividers and F5 with the row hidden: the row went hidden,
    shown, hidden, then a run showed it; the doc pane grew when it was hidden; the dividers moved
    and were stored; nothing ran but the one F5 run."""
    problems = []
    for name in ("toggle_bottom", "split_main", "split_doc", "doc", "bottom_row", "editor"):
        if not widget_rects(text, name):
            problems.append(f"{name} was never probed")
    if problems:
        return problems
    if layout_events(text) != ["HIDDEN", "SHOWN", "HIDDEN", "RUN", "SHOWN"]:
        problems.append(f"the bottom row's history is {layout_events(text)}, expected "
                        "HIDDEN, SHOWN, HIDDEN, then F5's RUN and SHOWN")
    started = [i for i, r in runs(text).items() if r.get("start")]
    if started != [1]:
        problems.append(f"expected exactly run 1 to start (nothing runs because of a toggle), got {started}")
    docs = [h for _, _, _, h in widget_rects(text, "doc")]
    if max(docs) - min(docs) < 150:
        problems.append(f"the doc pane's height never changed by 150 px (heights {sorted(set(docs))}): hiding "
                        "the bottom row must give it the column")
    mains = {x for x, _, _, _ in widget_rects(text, "split_main")}
    if len(mains) < 2:
        problems.append("the Explorer divider never moved (dragging it changed nothing)")
    splits = {y for _, y, _, _ in widget_rects(text, "split_doc")}
    if len(splits) < 3:
        problems.append("the doc divider never moved apart from the toggles")
    problems += _need(text, rf"^{P}:LAYOUT:SAVED:\d+:[01]\.\d+$", "the dragged layout was never written (LAYOUT:SAVED)")
    return problems


def judge_maximize(text: str) -> list[str]:
    """The window was maximized and restored: the doc pane and the editor grew by what the window
    did, the Explorer and its divider did not move, then they came back."""
    problems = []
    docs = widget_rects(text, "doc")
    editors = widget_rects(text, "editor")
    tabs = widget_rects(text, "explorer_tabs")
    if len(docs) < 3 or not editors or not tabs:
        return ["the doc pane, the editor or the Explorer was not probed after each resize"]
    widest = max(w for _, _, w, _ in docs)
    if widest - docs[0][2] < 400:
        problems.append(f"the doc pane went from {docs[0][2]} to at most {widest} px wide: it did not take "
                        "the maximized window's extra width")
    if max(h for _, _, _, h in editors) - editors[0][3] < 100:
        problems.append("the editor did not grow when the window did")
    if len({w for _, _, w, _ in tabs}) != 1:
        problems.append(f"the Explorer's width changed with the window: {sorted({w for _, _, w, _ in tabs})}")
    if abs(docs[-1][2] - docs[0][2]) > 2:
        problems.append(f"the doc pane did not come back to {docs[0][2]} px (it is {docs[-1][2]})")
    return problems


def judge_layout_stored(text: str) -> list[str]:
    """A later launch finds the layout the last one stored (an Explorer narrower than the default
    340, or the doc share changed), then pulling the hidden row's divider up shows the row."""
    found = re.search(rf"^{P}:LAYOUT:LOADED:(\d+):([01]\.\d+):(shown|hidden)$", text, re.M)
    if not found:
        return ["the stored layout was not found at start (LAYOUT:LOADED)"]
    problems = []
    if int(found.group(1)) == 340 and found.group(2) == "0.55":
        problems.append("the stored layout is the default: the dragged dividers were not stored")
    if layout_events(text) != ["HIDDEN", "SHOWN"]:
        problems.append(f"expected Ctrl+J to hide the row and the pulled divider to show it, got {layout_events(text)}")
    if runs(text):
        problems.append("a run started (nothing may run because of a toggle or a drag)")
    return problems


def judge_review(text: str) -> list[str]:
    """Three F5 presses (one cancelled), two runs: the pasted text asked, nothing ran on open."""
    seen = {i: r for i, r in runs(text).items() if r.get("start")}
    problems = []
    if sorted(seen) != [1, 2]:
        problems.append(f"expected exactly runs 1 and 2 to start, got {sorted(seen)}")
    if seen.get(1, {}).get("exit") != "0":
        problems.append("the confirmed run did not exit 0")
    return problems


SESSIONS = {
    "browse": judge_browse,
    "run": judge_run,
    "serve": judge_serve,
    "listen": judge_listen,
    "elevd": judge_elevd,
    "review": judge_review,
    "notice": judge_notice,
    "notice_stored": judge_notice_stored,
    "layout": judge_layout,
    "layout_stored": judge_layout_stored,
    "maximize": judge_maximize,
}


def judge_serial(name: str, text: str, names: dict[int, str] | None = None,
                 first_boot: bool = False) -> list[str]:
    """Every serial judge of session `name`."""
    allowed = EXPECTED_DENIED if name == "run" else set()
    problems = judge_integrity(text) + judge_install(text, first_boot) + judge_app(text) + judge_orphans(text)
    problems += judge_monitor(text)
    problems += judge_denials(text, names if names is not None else interface_names(), allowed)
    return problems + SESSIONS[name](text)


def judge_shots(directory: Path, expected: list[str]) -> list[str]:
    """Each named screenshot exists, is the screen's size and is not blank."""
    sys.path.insert(0, str(ROOT / "tools" / "screenshot"))
    import pngstats  # noqa: E402

    problems = []
    for name in expected:
        path = directory / f"shot_{name}.png"
        if not path.is_file():
            problems.append(f"screenshot {name} is missing")
            continue
        _, stats, failures = pngstats.analyse_file(
            str(path), SCREEN[0], SCREEN[1], 0.05, 8, None)
        problems += failures
    return problems
