#!/usr/bin/env python3
"""The Playground harness's own tests: the judge fails when it should.

A good serial log per session passes every judge; each defect a real run could show (the strip
missing, the app not started under its `tool:` label, a refused call of the app's own, a run
started by opening, an orphaned `rhai`, a dropped serial line, a blank screenshot) is made in
turn and must be reported. The checked-in sessions are current and take exactly the screenshots
`run.py` judges.

    python tools/playground/test_judge.py
"""

from __future__ import annotations

import json
import struct
import sys
import tempfile
import unittest
import zlib
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import judge as j  # noqa: E402
import run  # noqa: E402
import session  # noqa: E402

P = j.P
HASH = "0123456789abcdef"
NAMES = j.interface_names()
KEYD_ID = next(i for i, n in NAMES.items() if n == "os.lazy.keyd.v1")
CONFD_ID = next(i for i, n in NAMES.items() if n == "os.lazy.confd.v1")


def log(*lines: str) -> str:
    return "\n".join(lines) + "\n"


def start(n: int) -> str:
    return f"{P}:RUN:{n}:START:{HASH}"


def end(n: int, status: str) -> str:
    return f"{P}:RUN:{n}:EXIT:{status}:{HASH}"


def rhai_exit(n: int = 1) -> list[str]:
    return [f"PROC:EXIT tgid={20 + i} thread={20 + i}/fork status=0 sig=- by=- parent=20 cpu_ms=0 comm=rhai"
            for i in range(n)]


def frame(extra: list[str], rhais: int) -> list[str]:
    """What every session prints around its own markers."""
    return [
        "PKGD:POLICY:PASS tool:os.lazy.playground rules=9",
        "PKGD:PROVISION:INSTALL os.lazy.playground ticks=25",
        "PKGD:PROVISION:DONE installed=1 upgraded=0 kept=0 failed=0 free=1",
        "INIT:LAUNCH:PASS app=os.lazy.playground pid=20 session=1",
        "PKGD:LAUNCH:LABEL tool:os.lazy.playground pid=20",
        f"UI:RECT x=50 y=70 w=1180 h=660 name=window:{j.TITLE}",
        f"{P}:DEVTOOL:UP",
        f"{P}:UP:PASS",
        f"UI:WIDGET x=0 y=30 w=1180 h=24 name=devtool:strip window={j.TITLE}",
        *extra,
        f"{P}:QUIT:PASS",
        "INIT:LAUNCH:EXIT app=os.lazy.playground status=0",
        *rhai_exit(rhais),
    ]


def good(name: str) -> str:
    extras = {
        "browse": ([f"{P}:OPEN:os.lazy.confd.v1"], 0),
        "run": ([start(1), end(1, "0"), f"{P}:OPEN:os.lazy.confd.v1", start(2), end(2, "0"),
                 f"{P}:CALL:os.lazy.confd.v1.Info:OK", start(3), end(3, "0"),
                 f"{P}:CALL:os.lazy.confd.v1.Get:OK", start(4), end(4, "1"), start(5),
                 end(5, "stopped"),
                 f"LABEL:DENY label=tool:os.lazy.playground iface=0x{KEYD_ID:x} method=1"], 5),
        "serve": ([start(1), start(2), end(2, "0"), end(1, "stopped"), start(3)], 3),
        "listen": ([f"UI:RECT x=300 y=90 w=760 h=620 name=window:{j.TOPICS_TITLE}", f"{P}:TOPICS:UP:PASS",
                    f"UI:WIDGET x=0 y=30 w=760 h=24 name=devtool:strip window={j.TOPICS_TITLE}",
                    f"UI:WIDGET x=600 y=560 w=60 h=28 name=pub_go window={j.TOPICS_TITLE}",
                    f"{P}:LISTEN:time/tick", f"{P}:EVENT:time/tick:4", f"{P}:LISTEN:system/stats/memory",
                    f"{P}:EVENT:system/stats/memory:24", f"{P}:LISTEN:demo/pg/x",
                    f"{P}:EVENT:demo/pg/x:30", start(1), end(1, "0"), start(2), end(2, "0"),
                    f"{P}:TOPICS:QUIT:PASS"], 2),
        "elevd": (["XUID:PROMPT:UP", "ELEVD:REQUEST op=service.restart uid=1000 admin=admin outcome=granted",
                   start(1), end(1, "0")], 1),
        "review": ([f"{P}:REVIEW:ASK:clipboard", f"{P}:REVIEW:ASK:clipboard", start(1), end(1, "0"),
                    start(2), end(2, "0"), f"{P}:REVIEW:ASK:file", f"{P}:REVIEW:ASK:clipboard"], 2),
        "notice": ([f"UI:WIDGET x=1150 y=34 w=18 h=18 name=devtool:strip_dismiss window={j.TITLE}",
                    f"{P}:NOTICE:HIDDEN"], 0),
        "notice_stored": ([f"{P}:NOTICE:HIDDEN:stored", f"{P}:NOTICE:SHOWN"], 0),
        "maximize": ([widget("doc", 350, 130, 800, 211), widget("editor", 350, 440, 500, 250),
                      widget("explorer_tabs", 0, 90, 340, 600),
                      widget("doc", 350, 130, 1400, 400), widget("editor", 350, 600, 900, 380),
                      widget("doc", 350, 130, 800, 211)], 0),
        "layout": layout_log(),
        "layout_stored": layout_log(stored=True),
    }[name]
    return log(*frame(*extras))


def widget(name: str, x: int, y: int, w: int, h: int) -> str:
    return f"UI:WIDGET x={x} y={y} w={w} h={h} name={name} window={j.TITLE}"


def layout_log(stored: bool = False) -> tuple[list[str], int]:
    """A `layout` session's markers and probe lines (or the `layout_stored` ones)."""
    if stored:
        return ([f"{P}:LAYOUT:LOADED:260:0.40:shown", f"{P}:LAYOUT:BOTTOM:HIDDEN", f"{P}:LAYOUT:BOTTOM:SHOWN"], 0)
    return ([
        widget("toggle_bottom", 1100, 380, 80, 24), widget("editor", 350, 440, 500, 250),
        widget("bottom_row", 350, 410, 900, 290),
        widget("split_main", 345, 60, 5, 600), widget("split_doc", 350, 400, 800, 5),
        widget("doc", 350, 130, 800, 211),
        f"{P}:LAYOUT:BOTTOM:HIDDEN", widget("doc", 350, 130, 800, 540), widget("split_doc", 350, 665, 800, 5),
        f"{P}:LAYOUT:BOTTOM:SHOWN", widget("doc", 350, 130, 800, 211),
        widget("split_main", 225, 60, 5, 600), widget("split_doc", 350, 300, 800, 5),
        f"{P}:LAYOUT:SAVED:260:0.40",
        f"{P}:LAYOUT:BOTTOM:HIDDEN", start(1), end(1, "0"), f"{P}:LAYOUT:BOTTOM:SHOWN",
    ], 1)


def problems(name: str, text: str, first_boot: bool = True) -> list[str]:
    return j.judge_serial(name, text, NAMES, first_boot)


def png(path: Path, width: int, height: int, color: tuple[int, int, int]) -> None:
    row = b"\x00" + bytes(color) * width
    raw = row * height

    def chunk(kind: bytes, data: bytes) -> bytes:
        body = kind + data
        return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body) & 0xFFFFFFFF)

    path.write_bytes(b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0))
                     + chunk(b"IDAT", zlib.compress(raw)) + chunk(b"IEND", b""))


class GoodLogs(unittest.TestCase):
    def test_every_session_passes_its_own_good_log(self):
        for name in j.SESSIONS:
            self.assertEqual(problems(name, good(name)), [], name)

    def test_the_install_line_is_only_asked_of_the_first_boot(self):
        text = good("browse").replace("PKGD:PROVISION:INSTALL os.lazy.playground ticks=25\n", "")
        self.assertTrue(problems("browse", text, first_boot=True))
        self.assertEqual(problems("browse", text, first_boot=False), [])


class Defects(unittest.TestCase):
    def fails(self, name: str, text: str, needle: str):
        found = problems(name, text)
        self.assertTrue(any(needle in p for p in found), f"{needle!r} not in {found}")

    def test_the_app_must_run_as_a_developer_tool_under_its_label(self):
        text = good("browse")
        self.fails("browse", text.replace("PKGD:LAUNCH:LABEL tool:os.lazy.playground pid=20\n", ""), "tool: label")
        self.fails("browse", text.replace("PKGD:POLICY:PASS tool:os.lazy.playground rules=9\n", ""), "rules")
        self.fails("browse", text.replace(f"{P}:DEVTOOL:UP\n", ""), "developer tool")
        self.fails("browse", text.replace("name=devtool:strip", "name=other"), "strip")
        self.fails("browse", text.replace(f"name=window:{j.TITLE}", "name=window:Messenger Playground"), "titled")

    def test_the_app_must_start_and_quit_cleanly(self):
        text = good("browse")
        self.fails("browse", text.replace(f"{P}:UP:PASS\n", ""), "UP:PASS")
        self.fails("browse", text.replace(f"{P}:QUIT:PASS\n", ""), "QUIT:PASS")
        self.fails("browse", text.replace("status=0", "status=1"), "status 0")
        self.fails("browse", text + f"{P}:RUN:FAIL:boom\n", "RUN:FAIL")
        self.fails("browse", text + "LazyOS PANIC at x\n", "panic")

    def test_a_log_with_dropped_serial_lines_proves_nothing(self):
        self.fails("browse", good("browse") + "SERIAL:DROPPED bytes=10 total=10\n", "dropped")

    def test_an_unexpected_refusal_of_the_apps_own_calls_is_reported(self):
        text = good("browse") + f"LABEL:DENY label=tool:os.lazy.playground iface=0x{CONFD_ID:x} method=1\n"
        self.fails("browse", text, "unexpected LABEL:DENY")
        # A script's deny-listed call is only expected in the session that makes it.
        keyd = good("run").split("LABEL:DENY")[1]
        self.fails("browse", good("browse") + "LABEL:DENY" + keyd, "unexpected LABEL:DENY")
        # Another label's refusals are not the app's.
        other = good("browse") + f"LABEL:DENY label=app:os.lazy.x iface=0x{CONFD_ID:x} method=1\n"
        self.assertEqual(problems("browse", other), [])
        self.fails("browse", good("browse") + "LABEL:DENY label=tool:os.lazy.playground resolve=demo.x\n",
                   "unexpected LABEL:DENY")

    def test_closing_must_leave_no_rhai_behind(self):
        text = good("serve")
        orphan = text.replace("PROC:EXIT tgid=22 thread=22/fork status=0 sig=- by=- parent=20 cpu_ms=0 comm=rhai\n", "")
        self.assertNotEqual(text, orphan)
        self.fails("serve", orphan, "only 2 rhai processes exited")

    def test_run_session_defects(self):
        text = good("run")
        self.fails("run", text.replace(end(1, "0"), end(1, "1")), "run 1")
        self.fails("run", text.replace(f"{P}:CALL:os.lazy.confd.v1.Info:OK\n", ""), "Call form")
        self.fails("run", text.replace(f"{P}:CALL:os.lazy.confd.v1.Get:OK" + chr(10), ""), "with a path")
        self.fails("run", text.replace(end(4, "1"), end(4, "0")), "refused call must not succeed")
        self.fails("run", text.replace(end(5, "stopped"), end(5, "1")), "Stop did not end run 5")

    def test_serve_session_defects(self):
        text = good("serve")
        self.fails("serve", text.replace(end(2, "0"), end(2, "1")), "not answered")
        self.fails("serve", text.replace(end(1, "stopped"), end(1, "0")), "Stop did not end")
        self.fails("serve", text.replace(start(3), ""), "run 3")
        self.fails("serve", text.replace(start(3), start(3) + "\n" + end(3, "0")), "ended on its own")

    def test_listen_session_defects(self):
        text = good("listen")
        self.fails("listen", text.replace(f"{P}:EVENT:time/tick:4\n", ""), "no event arrived on time/tick")
        self.fails("listen", text.replace(f"{P}:LISTEN:demo/pg/x\n", ""), "never listened to demo/pg/x")
        self.fails("listen", text.replace(end(1, "0"), end(1, "1")), "Peek")
        self.fails("listen", text.replace(f"{P}:EVENT:demo/pg/x:30\n", ""), "no event arrived on demo/pg/x")

    def test_elevd_session_defects(self):
        text = good("elevd")
        self.fails("elevd", text.replace("XUID:PROMPT:UP\n", ""), "prompt")
        self.fails("elevd", text.replace("outcome=granted", "outcome=cancelled"), "did not grant")

    def test_nothing_may_run_on_open_or_on_a_declined_review(self):
        text = good("review")
        self.fails("review", text.replace(start(1), start(1) + "\n" + start(3), 1), "exactly runs 1 and 2")
        self.fails("review", text.replace(start(1) + "\n" + end(1, "0") + "\n", ""), "exactly runs 1 and 2")
        self.fails("review", text.replace(end(1, "0"), end(1, "1")), "confirmed run")

    def test_the_review_cards_must_be_shown_not_only_the_runs_counted(self):
        text = good("review")
        card = f"{P}:REVIEW:ASK:clipboard\n"
        self.fails("review", text.replace(card, "", 1), "review cards")
        self.fails("review", text.replace(card, ""), "review cards")
        self.fails("review", text.replace(f"{P}:REVIEW:ASK:file\n", ""), "review cards")
        # The deleted-typing case: the last card is the one that proves the gate held.
        last = text.rindex(card)
        self.fails("review", text[:last] + text[last + len(card):], "review cards")
        self.fails("review", text.replace("ASK:file", "ASK:typed"), "review cards")

    def test_the_topic_monitor_must_come_up_and_go_before_the_main_window(self):
        text = good("listen")
        self.fails("listen", text.replace(f"{P}:TOPICS:UP:PASS\n", ""), "never opened the Topic monitor")
        self.fails("listen", text.replace(f"{P}:TOPICS:QUIT:PASS\n", ""), "left open")
        quit_line = f"{P}:QUIT:PASS\n"
        late = text.replace(f"{P}:TOPICS:QUIT:PASS\n", "").replace(quit_line, quit_line + f"{P}:TOPICS:QUIT:PASS\n")
        self.fails("listen", late, "after the main window")
        self.fails("listen", text.replace(f"name=window:{j.TOPICS_TITLE}", "name=window:x"), "titled")
        self.fails("listen", text.replace(f"name=devtool:strip window={j.TOPICS_TITLE}", "name=other"), "strip was never probed")
        self.fails("listen", text.replace(f"name=pub_go window={j.TOPICS_TITLE}", f"name=pub_go window={j.TITLE}"), "controls were never probed")
        # A session that never opened it needs none of that.
        self.assertEqual(problems("browse", good("browse")), [])

    def test_notice_session_defects(self):
        text = good("notice")
        self.fails("notice", text.replace("name=devtool:strip_dismiss", "name=x"), "dismiss button")
        self.fails("notice", text.replace(f"{P}:NOTICE:HIDDEN\n", ""), "NOTICE:HIDDEN")
        text = good("notice_stored")
        self.fails("notice_stored", text.replace(f"{P}:NOTICE:HIDDEN:stored\n", ""), "not honoured")
        self.fails("notice_stored", text.replace(f"{P}:NOTICE:SHOWN\n", ""), "NOTICE:SHOWN")
        self.fails("notice_stored", text + f"{P}:NOTICE:HIDDEN\n", "dismissed again")

    def test_layout_session_defects(self):
        text = good("layout")
        self.fails("layout", text.replace("name=toggle_bottom", "name=x"), "toggle_bottom was never probed")
        self.fails("layout", text.replace("name=split_main", "name=x"), "split_main was never probed")
        self.fails("layout", text.replace(f"{P}:LAYOUT:BOTTOM:SHOWN\n", "", 1), "history")
        # A toggle that ran something.
        self.fails("layout", text.replace(f"{P}:LAYOUT:BOTTOM:HIDDEN\n", f"{P}:LAYOUT:BOTTOM:HIDDEN\n{start(7)}\n", 1),
                   "nothing runs because of a toggle")
        # Hiding the row gave the doc pane nothing.
        self.fails("layout", text.replace("w=800 h=540", "w=800 h=211"), "never changed by 150 px")
        self.fails("layout", text.replace("name=split_main", "name=split_main", 1).replace("x=225", "x=345"),
                   "Explorer divider never moved")
        self.fails("layout", text.replace(f"{P}:LAYOUT:SAVED:260:0.40\n", ""), "never written")
        # F5 did not reveal the row.
        gone = text.rsplit(f"{P}:LAYOUT:BOTTOM:SHOWN\n", 1)
        self.fails("layout", gone[0] + gone[1], "history")

    def test_layout_stored_session_defects(self):
        text = good("layout_stored")
        self.fails("layout_stored", text.replace(f"{P}:LAYOUT:LOADED:260:0.40:shown\n", ""), "not found at start")
        self.fails("layout_stored", text.replace("LOADED:260:0.40", "LOADED:340:0.55"), "default")
        self.fails("layout_stored", text.replace(f"{P}:LAYOUT:BOTTOM:SHOWN\n", ""), "pulled divider")
        self.fails("layout_stored", text.replace(f"{P}:LAYOUT:BOTTOM:HIDDEN\n", f"{P}:LAYOUT:BOTTOM:HIDDEN\n{start(1)}\n", 1),
                   "a run started")

    def test_maximize_session_defects(self):
        text = good("maximize")
        self.fails("maximize", text.replace("w=1400 h=400", "w=900 h=400"), "extra width")
        self.fails("maximize", text.replace("w=900 h=380", "w=500 h=250"), "editor did not grow")
        self.fails("maximize", text + widget("explorer_tabs", 0, 90, 500, 600) + "\n", "Explorer's width changed")
        head, tail = text.rsplit("w=800 h=211", 1)
        self.fails("maximize", head + "w=1000 h=211" + tail, "did not come back")

    def test_browse_must_open_an_interface(self):
        self.fails("browse", good("browse").replace(f"{P}:OPEN:os.lazy.confd.v1\n", ""), "OPEN")


class Screenshots(unittest.TestCase):
    def test_missing_blank_and_small_shots_fail(self):
        with tempfile.TemporaryDirectory() as tmp:
            d = Path(tmp)
            png(d / "shot_a.png", 1280, 720, (40, 60, 90))
            # A real screenshot has many colours: build one with bands.
            self.assertTrue(j.judge_shots(d, ["a", "missing"]))
            self.assertTrue(any("missing" in p for p in j.judge_shots(d, ["missing"])))
            png(d / "shot_small.png", 640, 360, (40, 60, 90))
            self.assertTrue(any("width" in p for p in j.judge_shots(d, ["small"])))
            png(d / "shot_black.png", 1280, 720, (0, 0, 0))
            self.assertTrue(any("non-background" in p for p in j.judge_shots(d, ["black"])))


class Sessions(unittest.TestCase):
    def test_the_checked_in_sessions_are_current(self):
        self.assertEqual(session.main(["--check"]), 0)

    def test_each_session_takes_the_screenshots_the_harness_judges(self):
        for name, (script, shots) in run.SESSIONS.items():
            steps = json.loads((session.EXAMPLES / script).read_text())
            taken = [s["shot"] for s in steps if "shot" in s]
            self.assertEqual(taken, shots, name)
            self.assertIn(script, session.SESSIONS)
            self.assertIn(name, j.SESSIONS)

    def test_every_session_ends_with_ctrl_q_and_every_click_names_a_widget(self):
        for script in session.SESSIONS:
            steps = json.loads((session.EXAMPLES / script).read_text())
            self.assertEqual(steps[-1], {"quit": True}, script)
            self.assertTrue(any(s.get("until") == "PLAYGROUND:QUIT:PASS" for s in steps), script)
            for step in steps:
                if "click_at" in step:
                    self.assertIn(step["click_at"]["window"], (j.TITLE, j.TOPICS_TITLE))
                    self.assertIn("widget", step["click_at"])


if __name__ == "__main__":
    unittest.main()
