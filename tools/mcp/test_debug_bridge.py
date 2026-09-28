#!/usr/bin/env python3
"""Unit tests for the parts of the MCP debug bridge that need no QEMU."""

import json
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from debug_bridge import marker_pattern  # noqa: E402


class MarkerPatternTests(unittest.TestCase):
    payload = {"live": 3, "rows": [1, 2]}

    def line(self, marker: str = "TASK_SNAPSHOT") -> str:
        return f"MCP:{marker}:{json.dumps(self.payload)}"

    def test_finds_line_at_end_of_buffer(self):
        match = marker_pattern("TASK_SNAPSHOT").search(self.line() + "\n")
        self.assertIsNotNone(match)
        self.assertEqual(json.loads(match.group(1)), self.payload)

    def test_finds_line_followed_by_shell_prompt(self):
        # The regression: serial output after the payload line (the prompt, the
        # next command) used to hide it.
        buffer = "tasks-json\n" + self.line() + "\nlazyos$ "
        match = marker_pattern("TASK_SNAPSHOT").search(buffer)
        self.assertIsNotNone(match)
        self.assertEqual(json.loads(match.group(1)), self.payload)

    def test_handles_crlf_serial_lines(self):
        match = marker_pattern("TASK_SNAPSHOT").search(self.line() + "\r\nprompt")
        self.assertIsNotNone(match)
        self.assertEqual(json.loads(match.group(1)), self.payload)

    def test_ignores_other_markers(self):
        self.assertIsNone(
            marker_pattern("TASK_SNAPSHOT").search(self.line("FABRIC_STATS") + "\n")
        )


if __name__ == "__main__":
    unittest.main()
