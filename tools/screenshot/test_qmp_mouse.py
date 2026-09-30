"""Unit tests for the relative mouse moves in qemu_qmp.py (no QEMU needed):
python tools/screenshot/test_qmp_mouse.py"""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import qemu_qmp  # noqa: E402


class StepTests(unittest.TestCase):
    def test_a_small_move_is_one_step(self):
        self.assertEqual(qemu_qmp.mouse_move_steps(10, -20), [(10, -20)])

    def test_a_zero_move_sends_nothing(self):
        self.assertEqual(qemu_qmp.mouse_move_steps(0, 0), [])

    def test_a_large_move_is_split_at_one_packet_per_step(self):
        steps = qemu_qmp.mouse_move_steps(850, 400)
        self.assertTrue(all(abs(x) <= 127 and abs(y) <= 127 for x, y in steps))
        self.assertEqual(sum(x for x, _ in steps), 850)
        self.assertEqual(sum(y for _, y in steps), 400)
        self.assertEqual(len(steps), 7)  # 850 / 127 -> 7 steps

    def test_the_slam_into_a_corner_is_exact_in_both_directions(self):
        for dx, dy in [(-300, -300), (300, 300), (-1, 700), (127, -128), (-127, 127)]:
            steps = qemu_qmp.mouse_move_steps(dx, dy)
            self.assertEqual((sum(x for x, _ in steps), sum(y for _, y in steps)), (dx, dy))
            self.assertTrue(all(abs(x) <= 127 and abs(y) <= 127 for x, y in steps))

    def test_axes_finish_independently(self):
        # 400 in x needs 4 steps, 10 in y only the first.
        steps = qemu_qmp.mouse_move_steps(400, 10)
        self.assertEqual(steps[0], (127, 10))
        self.assertTrue(all(y == 0 for _, y in steps[1:]))


class SendTests(unittest.TestCase):
    def test_mouse_move_sends_one_event_batch_per_step_in_order(self):
        sent = []
        client = qemu_qmp.Qmp.__new__(qemu_qmp.Qmp)
        client.send_events = lambda events, device=None: sent.append(events)
        client.mouse_move(300, -300, delay=0)
        totals = {"x": 0, "y": 0}
        for events in sent:
            for event in events:
                totals[event["data"]["axis"]] += event["data"]["value"]
        self.assertEqual(totals, {"x": 300, "y": -300})
        self.assertEqual(len(sent), 3)


if __name__ == "__main__":
    unittest.main()
