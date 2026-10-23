#!/usr/bin/env python3
"""The hung-page judge fails when it should (`hang_judge.py`)."""

from __future__ import annotations

import re
import sys
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import hang_judge as h  # noqa: E402
import hang_session  # noqa: E402
import judge as j  # noqa: E402

P = h.P
#: The engine's markers behind webd's relay prefix.
N = j.NOTE
CLIENT = j.CLIENT_LABEL


def good(quota: tuple[int, ...] = (301, 303, 304, 306)) -> str:
    """The log of the session: Wait, the toolbar's Reload, CYCLES more bar Reloads, then Stop (webd reports
    the view lost), then the next load, which makes a new view and a new page process, and the close. Every
    ended web process has its proxy kept and counted (patch 0021). The kernel gives a relaunched process
    the lowest free id, so every second relaunch has the dead process's own id. `quota` is the user
    memory charge read after each load that follows a Reload."""
    lines = [
        j.WEBD_UP, f"WEBD:VIEW:CREATE client={CLIENT} view=7", f"{P}:UP:PASS",
        f"{N}PROCESS:LAUNCH:network:32", f"{N}PROCESS:LAUNCH:web:30",
        f"{N}NETWORKPROCESS:UP:32", f"{N}WEBPROCESS:UP:30", f"{N}INFO:QUOTA:user_memory_mib=300:limit_mib=8192",
        f"{P}:LOAD:{h.PAGE}", f"{P}:TITLE:{h.READY}",
    ]
    pid = 30
    kept = 0
    readings = iter(quota)
    relaunches = 0

    def relaunch() -> None:
        nonlocal pid, relaunches
        relaunches += 1
        if relaunches % 2:
            pid += 6

    def cycle(answer: str, wait_first: bool) -> None:
        nonlocal kept
        lines.extend([f"{P}:TITLE:{h.SPINNING}", f"{N}WEBPROCESS:UNRESPONSIVE:{pid}", f"{P}:HANG:SHOW"])
        if wait_first:
            lines.extend([f"{P}:HANG:ANSWER:wait", f"{P}:HANG:HIDE"])
        lines.extend([
            f"{P}:HANG:ANSWER:{answer}", f"{N}WEBPROCESS:RECOVER:{pid}:{answer}",
            f"{N}PROCESS:TERMINATE:web:{pid}:group=0:pid=-1", f"{N}PROCESS:KEPT:{pid}:{kept + 1}",
            f"{N}WEBPROCESS:GONE:{pid}:requested", f"{N}PROCESS:EXIT:web:{pid}:signal=9",
        ])
        kept += 1
        if answer == "reload":
            relaunch()
            lines.extend([f"{N}PROCESS:LAUNCH:web:{pid}", f"{N}WEBPROCESS:UP:{pid}", f"{P}:LOAD:{h.PAGE}",
                          f"{P}:TITLE:{h.READY}", f"{N}INFO:QUOTA:user_memory_mib={next(readings)}:limit_mib=8192"])
        else:
            lines.append(f"WEBD:LOST view=7 reason={h.STOP_REASON}")

    cycle("reload", True)
    for _ in range(h.CYCLES):
        cycle("reload", False)
    cycle("stop", False)
    # The next load: a new view, a new page process, the page again.
    relaunch()
    lines.extend([f"{P}:NAV:{h.PAGE}", f"WEBD:VIEW:CREATE client={CLIENT} view=8",
                  f"{N}PROCESS:LAUNCH:web:{pid}", f"{N}WEBPROCESS:UP:{pid}",
                  f"{P}:LOAD:{h.PAGE}", f"{P}:TITLE:{h.READY}"])
    # The window closes: webd destroys both views, the last page process ends, the network process stays.
    lines.extend(["WEBD:VIEW:DESTROY view=7", "WEBD:VIEW:DESTROY view=8", f"{N}PROCESS:EXIT:web:{pid}:signal=9",
                  f"{P}:QUIT:PASS"])
    return "\n".join(lines) + "\n"


class HangJudge(unittest.TestCase):
    def test_a_good_run_passes(self) -> None:
        self.assertEqual(h.judge_hang(good()), [])

    def test_each_step_is_needed(self) -> None:
        for gone in (f"{P}:HANG:SHOW\n", f"{P}:HANG:ANSWER:wait\n", f"{P}:HANG:ANSWER:stop\n",
                     f"{N}WEBPROCESS:RECOVER:30:reload\n", "WEBD:LOST view=7 reason=other\n",
                     "WEBD:VIEW:CREATE client=app:os.lazy.lazyweb view=8\n", "WEBD:VIEW:DESTROY view=8\n",
                     j.WEBD_UP + "\n"):
            self.assertTrue(h.judge_hang(good().replace(gone, "")), gone)

    def test_the_stops_exit_and_the_lost_view_may_come_in_either_order(self) -> None:
        # WebKit reports a termination it asked for at once; the reaper prints the exit when the kernel reaps
        # the killed child: a run of 2026-10-22 printed the loss four lines before the exit.
        lines = good().splitlines()
        lost = max(i for i, line in enumerate(lines) if line.startswith("WEBD:LOST view=7"))
        exit_at = max(i for i in range(lost) if lines[i].startswith(f"{N}PROCESS:EXIT:web:"))
        swapped = lines[:exit_at] + lines[exit_at + 1:lost + 1] + [lines[exit_at]] + lines[lost + 1:]
        self.assertEqual(h.judge_hang("\n".join(swapped) + "\n"), [])
        # The Stop's process must still end.
        without = lines[:exit_at] + lines[exit_at + 1:]
        self.assertTrue(any("Stop ended" in p for p in h.judge_hang("\n".join(without) + "\n")))

    def test_the_stop_is_proved_without_the_old_failure_marker(self) -> None:
        # webd may or may not relay FAIL:webprocess:stopped: the RECOVER, the process's end and the lost view prove it.
        self.assertEqual(h.judge_hang(good()), [])
        with_it = good().replace("WEBD:LOST", f"{N}FAIL:webprocess:stopped\nWEBD:LOST")
        self.assertEqual(h.judge_hang(with_it), [])

    def test_the_kept_proxies_are_judged_when_webd_relays_them_and_not_required(self) -> None:
        text = re.sub(rf"{re.escape(N)}PROCESS:KEPT:[^\n]*\n", "", good())
        self.assertEqual(h.judge_hang(text), [])
        self.assertTrue(any("kept proxies" in p for p in h.judge_hang(good().replace(f"{N}PROCESS:KEPT:30:1", f"{N}PROCESS:KEPT:0:1"))))
        self.assertTrue(any("count up" in p for p in h.judge_hang(good().replace(f"{N}PROCESS:KEPT:30:1", f"{N}PROCESS:KEPT:30:3"))))

    def test_a_relaunch_that_took_the_dead_processs_id_is_still_announced(self) -> None:
        # The good log has relaunches on the same id (the second, the fourth, ...): each must have its LAUNCH.
        text = good()
        self.assertGreaterEqual(len(re.findall(rf"{re.escape(N)}PROCESS:LAUNCH:web:36\n", text)), 2)
        once = text.replace(f"{N}PROCESS:LAUNCH:web:36\n", "", 1)
        self.assertTrue(any("launched" in p for p in h.judge_hang(once)), h.judge_hang(once))
        # ...and one LAUNCH of a process nobody launched again is not enough either.
        self.assertTrue(any("launched" in p for p in h.judge_hang(re.sub(rf"{re.escape(N)}PROCESS:LAUNCH:web:\d+\n", "", text))))

    def test_the_next_load_after_stop_makes_a_new_view_and_a_process(self) -> None:
        # No new view after the loss: the page stays blank.
        text = good().replace("WEBD:VIEW:CREATE client=app:os.lazy.lazyweb view=8\n", "")
        self.assertTrue(h.judge_hang(text))
        # A second view that never got a page process.
        quiet = re.sub(rf"{re.escape(N)}PROCESS:LAUNCH:web:\d+\n{re.escape(N)}WEBPROCESS:UP:\d+\n{re.escape(P)}:LOAD:[^\n]*\n"
                       rf"{re.escape(P)}:TITLE:{h.READY}\n(?=WEBD:VIEW:DESTROY view=7)", "", good())
        self.assertTrue(h.judge_hang(quiet))

    def test_a_lost_view_for_any_other_reason_is_not_the_windows_doing(self) -> None:
        for reason in ("crashed", "memory", "refused"):
            text = good().replace("reason=other", f"reason={reason}")
            self.assertTrue(any(reason in p for p in h.judge_hang(text)), reason)
        two = good().replace("WEBD:VIEW:CREATE client=app:os.lazy.lazyweb view=8",
                             "WEBD:LOST view=8 reason=crashed\nWEBD:VIEW:CREATE client=app:os.lazy.lazyweb view=8")
        self.assertTrue(any("one lost view" in p for p in h.judge_hang(two)))

    def test_a_process_that_is_not_freed_each_reload_fails_the_quota_bound(self) -> None:
        self.assertEqual(h.judge_hang(good((261, 269, 276, 281))), [])
        # The log that showed the uncollected SIGKILLed process: a step of 120 MiB.
        self.assertTrue(any("not freed" in p for p in h.judge_hang(good((261, 269, 276, 401)))))
        self.assertTrue(any("not freed" in p for p in h.judge_hang(good((300, 420, 540, 660)))))

    def test_the_quota_readings_that_webd_printed_are_all_that_count(self) -> None:
        # webd rates the lines: two readings are enough for a slope, and a few missing ones are not a fault.
        thin = good()
        for _ in range(2):
            thin = re.sub(rf"{re.escape(N)}INFO:QUOTA:user_memory_mib=3\d\d:limit_mib=8192\n(?=.*{re.escape(N)}INFO:QUOTA)",
                          "", thin, count=1, flags=re.S)
        self.assertEqual(h.judge_hang(thin), [])

    def test_readings_not_taken_after_an_ending_do_not_count(self) -> None:
        # No readings at all passed silently before.
        text = re.sub(rf"{re.escape(N)}INFO:QUOTA:[^\n]*\n", "", good())
        self.assertTrue(any("user memory readings" in p for p in h.judge_hang(text)))
        # Readings that are all taken before the first ending are not evidence of anything.
        early = f"{N}PROCESS:LAUNCH:web:30\n" + "".join(f"{N}INFO:QUOTA:user_memory_mib=300:limit_mib=8192\n" for _ in range(6))
        self.assertEqual(h.quota_after_exits(early), [])
        self.assertEqual(h.quota_after_exits(good()), [301, 303, 304, 306])
        # The old prefix is not the engine's.
        self.assertEqual(h.quota_after_exits(good().replace(N, f"{P}:")), [])

    def test_a_process_ended_by_a_crash_is_not_the_windows_doing(self) -> None:
        text = good().replace(f"{N}WEBPROCESS:GONE:36:requested", f"{N}WEBPROCESS:GONE:36:crash")
        self.assertTrue(any("crash" in p for p in h.judge_hang(text)))

    def test_wait_must_not_end_the_process(self) -> None:
        text = good().replace(f"{P}:HANG:HIDE", f"{P}:HANG:HIDE\n{N}WEBPROCESS:RECOVER:30:stop", 1)
        self.assertTrue(any("Wait ended" in p for p in h.judge_hang(text)))

    def test_a_hung_process_that_never_ended_fails(self) -> None:
        text = good().replace(f"{N}PROCESS:EXIT:web:30:signal=9\n", "", 1)
        self.assertTrue(any("never ended" in p or "never seen to end" in p for p in h.judge_hang(text)), h.judge_hang(text))

    def test_a_process_left_behind_or_a_view_not_destroyed_fails(self) -> None:
        last = re.findall(rf"{re.escape(N)}PROCESS:EXIT:web:\d+:signal=9\n(?={re.escape(P)}:QUIT)", good())[0]
        self.assertTrue(any("never seen to end" in p for p in h.judge_hang(good().replace(last, ""))))
        self.assertTrue(any("never destroyed" in p for p in h.judge_hang(good().replace("WEBD:VIEW:DESTROY view=8\n", ""))))
        alive = good().replace(f"{P}:QUIT:PASS", f"{P}:CHILD:curl:ALIVE\n{P}:QUIT:PASS")
        self.assertTrue(h.judge_hang(alive))

    def test_the_example_session_is_current(self) -> None:
        self.assertEqual(hang_session.EXAMPLE.read_text(encoding="utf-8"), hang_session.render(hang_session.hang()))

    def test_the_session_waits_on_the_engines_markers_as_webd_relays_them(self) -> None:
        waits = [step.get("wait_for", "") for step in hang_session.hang()]
        self.assertIn(f"{N}WEBPROCESS:UNRESPONSIVE:", waits)
        self.assertIn(f"{N}PROCESS:EXIT:web:", waits)
        self.assertIn("WEBD:LOST view=", waits)
        self.assertFalse([w for w in waits if w.startswith(f"{P}:WEBPROCESS")], "an engine marker under the old prefix")


if __name__ == "__main__":
    unittest.main()
