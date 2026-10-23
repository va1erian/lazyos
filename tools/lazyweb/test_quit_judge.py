#!/usr/bin/env python3
"""The quit judge fails when it should (`judge.judge_quit`, `webd_log.process_history`): a window that did not
close cleanly, a `curl` child that outlived it, a view webd never destroyed, a page process nobody saw end, a
terminate that signalled nothing. The network process is webd's and is not asked to end. These tests also
run with `test_judge.py`.

    python tools/lazyweb/test_quit_judge.py
"""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import judge as j  # noqa: E402
from judge_logs import CREATE, DESTROY, N, P, good_first  # noqa: E402


class QuitTests(unittest.TestCase):
    def test_a_good_quit_leaves_webds_network_process_running(self) -> None:
        text = good_first()
        self.assertIn(f"{N}PROCESS:LAUNCH:network:42", text)
        self.assertNotIn("PROCESS:EXIT:network", text)
        self.assertEqual(j.judge_quit(text), [])

    def test_a_page_process_that_outlived_the_window_fails_the_run(self) -> None:
        alive = good_first().replace(f"{N}PROCESS:EXIT:web:41:signal=9\n", "")
        problems = j.judge_quit(alive)
        self.assertTrue(any("web process 41" in p and "never seen to end" in p for p in problems), problems)

    def test_a_curl_child_that_outlived_the_window_fails_the_run(self) -> None:
        text = good_first().replace(f"{P}:QUIT:PASS", f"{P}:CHILD:curl:ALIVE\n{P}:QUIT:PASS")
        self.assertTrue(any("outlived" in p and "curl" in p for p in j.judge_quit(text)))

    def test_a_dirty_quit_fails(self) -> None:
        self.assertTrue(j.judge_quit(good_first().replace("QUIT:PASS", "QUIT:FAIL")))

    def test_a_view_webd_never_destroyed_fails(self) -> None:
        text = good_first().replace(f"{DESTROY}\n", "")
        self.assertTrue(any("view 7" in p and "never destroyed" in p for p in j.judge_quit(text)))
        # A view of another client is not the window's to destroy.
        other = good_first().replace(CREATE, CREATE + "\nWEBD:VIEW:CREATE client=app:os.lazy.other view=8")
        self.assertEqual(j.judge_quit(other), [])

    def test_the_shutdowns_extended_exit_line_is_accepted(self) -> None:
        text = good_first().replace("PROCESS:EXIT:web:41:signal=9", "PROCESS:EXIT:web:41:status=0:wait=41")
        self.assertEqual(j.judge_quit(text), [])

    def test_a_replaced_web_process_that_ended_earlier_is_accounted_for(self) -> None:
        # Page recovered: 41 ended, 44 took its place (a Reload), and 44 ends with the view.
        first = f"{N}PROCESS:EXIT:web:41:signal=9"
        second = f"{N}PROCESS:LAUNCH:web:44\n{N}WEBPROCESS:UP:44"
        text = good_first().replace(first, f"{first}\n{second}")
        self.assertTrue(any("web process 44" in p for p in j.judge_quit(text)), "44 was never seen to end")
        self.assertEqual(j.judge_quit(text.replace(second, f"{second}\n{N}PROCESS:EXIT:web:44:signal=9")), [])

    def test_a_network_process_webd_replaced_need_not_end(self) -> None:
        # WebKit gave up on the first network process and webd started another: both are webd's.
        text = good_first().replace(f"{N}NETWORKPROCESS:UP:42",
                                    f"{N}NETWORKPROCESS:UP:42\n{N}PROCESS:EXIT:network:42:signal=9\n"
                                    f"{N}PROCESS:LAUNCH:network:44\n{N}NETWORKPROCESS:UP:44")
        self.assertEqual(j.judge_quit(text), [])

    def test_a_terminate_that_sent_nothing_to_a_live_process_fails(self) -> None:
        sent = f"{N}PROCESS:TERMINATE:web:41:group=0:pid=-1"
        end = f"{N}PROCESS:EXIT:web:41:signal=9"
        self.assertEqual(j.judge_quit(good_first().replace(end, f"{sent}\n{end}")), [])
        lost = f"{N}PROCESS:TERMINATE:web:41:group=3:pid=3"
        self.assertTrue(j.judge_quit(good_first().replace(end, f"{lost}\n{end}")))
        # The group is gone but the process itself was signalled: sent.
        fallback = f"{N}PROCESS:TERMINATE:web:41:group=3:pid=0"
        self.assertEqual(j.judge_quit(good_first().replace(end, f"{fallback}\n{end}")), [])
        # Nothing to signal once it has ended: not a fault.
        after = f"{end}\n{N}PROCESS:TERMINATE:web:41:group=-1:pid=-1"
        self.assertEqual(j.judge_quit(good_first().replace(end, after)), [])

    def test_a_reused_id_is_a_new_process_not_the_old_ones_end(self) -> None:
        # Slot 42 was a network process that ended; a web process then took the slot. Its launch is not
        # accounted for by the earlier exit, and an exit of the wrong type is a fault.
        base = good_first()
        exit_web = f"{N}PROCESS:EXIT:web:41:signal=9"
        text = base.replace(exit_web, f"{N}PROCESS:EXIT:network:42:status=0\n{N}PROCESS:LAUNCH:web:42\n"
                                      f"{N}PROCESS:EXIT:web:42:signal=9\n{exit_web}")
        self.assertEqual(j.judge_quit(text), [])
        text = base.replace(exit_web, f"{N}PROCESS:EXIT:network:42:status=0\n{N}PROCESS:LAUNCH:web:42\n{exit_web}")
        self.assertTrue(any("never seen to end" in p for p in j.judge_quit(text)))
        text = base.replace(exit_web, f"{N}PROCESS:EXIT:network:42:status=0\n{N}PROCESS:LAUNCH:web:42\n"
                                      f"{N}PROCESS:EXIT:network:42:gone\n{N}PROCESS:EXIT:web:42:signal=9\n{exit_web}")
        self.assertTrue(any("not live" in p or "launched as web" in p for p in j.judge_quit(text)))

    def test_a_launch_of_an_id_still_live_and_a_second_exit_are_faults(self) -> None:
        end = f"{N}PROCESS:EXIT:web:41:signal=9"
        text = good_first().replace(end, f"{N}PROCESS:LAUNCH:network:42\n{end}")
        self.assertTrue(any("still live" in p for p in j.judge_quit(text)))
        text = good_first().replace(f"{P}:QUIT:PASS", f"{end}\n{P}:QUIT:PASS")
        self.assertTrue(any("not live" in p for p in j.judge_quit(text)))

    def test_the_old_prefix_is_not_the_engines(self) -> None:
        # Only webd's relay counts as an engine line: a `LAZYWEB:` one (the in-process engine's form) is nothing.
        text = good_first().replace(f"{N}PROCESS:EXIT:web:41:signal=9", f"{P}:PROCESS:EXIT:web:41:signal=9")
        self.assertTrue(any("never seen to end" in p for p in j.judge_quit(text)))

    def test_a_start_without_both_processes_a_view_or_webd_fails(self) -> None:
        base = f"{j.WEBD_UP}\n{CREATE}\n{P}:UP:PASS\n{N}WEBPROCESS:UP:1\n{N}NETWORKPROCESS:UP:2\n"
        self.assertEqual([p for p in j.judge_start(base)], [])
        self.assertTrue(any("NETWORKPROCESS" in p for p in j.judge_start(base.replace(f"{N}NETWORKPROCESS:UP:2\n", ""))))
        self.assertTrue(any("WEBPROCESS" in p for p in j.judge_start(base.replace(f"{N}WEBPROCESS:UP:1\n", ""))))
        self.assertTrue(any("UP:PASS" in p for p in j.judge_start(base.replace(f"{P}:UP:PASS\n", ""))))
        self.assertTrue(any("WEBD:VIEW:CREATE" in p for p in j.judge_start(base.replace(f"{CREATE}\n", ""))))
        self.assertTrue(any(j.WEBD_UP in p for p in j.judge_start(base.replace(f"{j.WEBD_UP}\n", ""))))
        # The process lines under the old prefix do not count.
        old = base.replace(N, f"{P}:")
        self.assertTrue(any("WEBPROCESS" in p for p in j.judge_start(old)))

    def test_a_lost_view_fails_a_normal_boot(self) -> None:
        text = good_first() + "WEBD:LOST view=7 reason=crashed\n"
        self.assertTrue(any("lost a view" in p for p in j.judge_start(text)))

    def test_an_engine_failure_is_a_failed_load_unless_it_is_the_planned_one(self) -> None:
        # webd names no address in a provisional failure: the client's own FAIL judges that load.
        text = good_first() + f"{P}:FAIL:Could not load http://elsewhere/: nope\n"
        self.assertTrue(any("elsewhere" in p for p in j.judge_pages(text)))
        planned = good_first() + f"{N}FAIL:provisional::Server returned nothing (code 52)\n"
        self.assertEqual(j.judge_pages(planned), [])
        crashed = good_first() + f"{N}FAIL:webprocess:crashed\n"
        self.assertTrue(any("webprocess:crashed" in p for p in j.judge_pages(crashed)))

    def test_the_engines_timing_lines_are_read_through_webd(self) -> None:
        text = (f"{N}INFO:TIME:firstpaint=1:sincecommit=2:sincenav=33\n"
                f"{N}INFO:TIME:nav=1:start=2:commit=3:finish=4:load=55\n"
                f"{P}:INFO:TIME:firstpaint=1:sincecommit=2:sincenav=99\n")
        self.assertEqual(j.first_paint_ms(text), [33])
        self.assertEqual(j.load_ms(text), [55])


if __name__ == "__main__":
    unittest.main()
