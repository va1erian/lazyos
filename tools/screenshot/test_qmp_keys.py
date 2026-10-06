"""Unit tests for the PS/2 batching in qemu_keys.py (no QEMU needed):
python tools/screenshot/test_qmp_keys.py"""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import qemu_keys  # noqa: E402
import qemu_qmp  # noqa: E402


def keys(text: str) -> list[dict]:
    return [event for ch in text for event in qemu_keys.char_events(ch)]


def cost(batch: list[dict]) -> int:
    return sum(qemu_keys._ps2_cost(event) for event in batch)


class BatchTests(unittest.TestCase):
    def test_a_small_list_stays_one_call(self):
        events = qemu_keys.named_key_events("left") + keys("Ab")
        self.assertEqual(qemu_keys.ps2_batches(events), [events])

    def test_no_batch_overflows_the_queue_and_order_is_kept(self):
        events = keys("The quick brown fox, 123!")
        batches = qemu_keys.ps2_batches(events)
        self.assertGreater(len(batches), 1)
        self.assertTrue(all(cost(b) <= qemu_keys.PS2_QUEUE_BYTES for b in batches))
        self.assertEqual([e for b in batches for e in b], events)

    def test_pause_costs_its_six_byte_make(self):
        pause = qemu_keys.named_key_down_events("pause")
        events = pause * 3
        self.assertEqual([len(b) for b in qemu_keys.ps2_batches(events)], [2, 1])

    def test_mouse_events_cost_nothing(self):
        moves = qemu_qmp.mouse_move_events(5, 5) * 40
        self.assertEqual(qemu_keys.ps2_batches(moves), [moves])

    def test_empty_sends_nothing(self):
        self.assertEqual(qemu_keys.ps2_batches([]), [])


class SendTests(unittest.TestCase):
    def test_send_events_splits_and_pauses(self):
        sent: list[list[dict]] = []

        class Fake(qemu_qmp.Qmp):
            def __init__(self):  # no socket
                self.key_interval = 0.0
                self._last_key_call = 0.0

            def execute(self, command, **arguments):
                assert command == "input-send-event"
                sent.append(arguments["events"])
                return {}

        events = keys("abcdefghijkl")  # 24 events, 24 bytes
        original = qemu_qmp.PS2_DRAIN_SECONDS
        try:
            qemu_qmp.PS2_DRAIN_SECONDS = 0
            Fake().send_events(events)
        finally:
            qemu_qmp.PS2_DRAIN_SECONDS = original
        self.assertEqual([len(b) for b in sent], [8, 8, 8])


if __name__ == "__main__":
    unittest.main()
