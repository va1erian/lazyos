#!/usr/bin/env python3
"""cpu_profile.py's aggregation, with a stand-in addr2line."""

from __future__ import annotations

import subprocess
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import cpu_profile as prof  # noqa: E402

# (seconds, cpl, cr3, rip)
ROWS = (
    [(0.0, 3, 0x1000, 0x400010)] * 6        # the web process, in `paint`
    + [(0.0, 3, 0x1000, 0x400020)] * 3      # ... in `paint` again, another address
    + [(0.0, 3, 0x1000, 0x500000)] * 1      # ... in `decode`
    + [(0.0, 3, 0x2000, 0x400010)] * 2      # another process
    + [(0.0, 0, 0x1000, 0xFFFF800000001000)] * 5  # the kernel
)


def fake_addr2line(command, **kwargs):
    names = {"0x400010": ("paint", "a.cpp:1"), "0x400020": ("paint", "a.cpp:9"), "0x500000": ("decode", "b.cpp:2")}
    out = []
    for address in command[5:]:
        out += list(names.get(address, ("??", "??:0")))
    return subprocess.CompletedProcess(command, 0, "\n".join(out) + "\n", "")


class Profile(unittest.TestCase):
    def test_the_busiest_user_space_is_chosen_and_addresses_add_up_per_function(self):
        text = "\n".join(prof.profile(ROWS, "WebProcess", "vol", None, 5, run=fake_addr2line))
        self.assertIn("18 samples, 12 in user mode", text.replace("{} samples".format(len(ROWS)), "18 samples"))
        self.assertIn("process cr3=1000: 10 samples over 3 addresses", text)
        self.assertRegex(text, r"90\.0%\s+9\s+paint")
        self.assertRegex(text, r"10\.0%\s+1\s+decode")

    def test_another_process_can_be_named(self):
        text = "\n".join(prof.profile(ROWS, "WebProcess", "vol", 0x2000, 5, run=fake_addr2line))
        self.assertIn("process cr3=2000: 2 samples", text)
        self.assertRegex(text, r"100\.0%\s+2\s+paint")

    def test_no_user_samples_is_said(self):
        kernel_only = [(0.0, 0, 0x1000, 0xFFFF800000001000)]
        self.assertIn("no user-mode samples", "\n".join(prof.profile(kernel_only, "WebProcess", "vol", None, 5, run=fake_addr2line)))


class Callers(unittest.TestCase):
    def test_the_first_other_function_on_the_stack_is_the_caller(self):
        names = {"0x600000": ("memcpy", "m.zig:1"), "0x400010": ("paint", "a.cpp:1"),
                 "0x500000": ("decode", "b.cpp:2")}

        def addr2line(command, **kwargs):
            out = []
            for address in command[5:]:
                out += list(names.get(address, ("??", "??:0")))
            return subprocess.CompletedProcess(command, 0, "\n".join(out) + "\n", "")

        rows = (
            # memcpy called from paint (a data word and memcpy's own address come first)
            [(0.0, 3, 0x1000, 0x600000, [0x7FFF_0000_1234, 0x600000, 0x400010, 0x500000])] * 3
            + [(0.0, 3, 0x1000, 0x600000, [0x500000])]          # ... from decode
            + [(0.0, 3, 0x1000, 0x600000, [])]                  # an older recording: no stack
            + [(0.0, 3, 0x1000, 0x400010, [0x500000])] * 4      # not in memcpy
            + [(0.0, 3, 0x2000, 0x600000, [0x400010])] * 9      # another process
        )
        text = "\n".join(prof.callers(rows, "lazywebplus-app", "vol", 0x1000, "memcpy", 5, run=addr2line))
        self.assertIn("5 samples in functions matching 'memcpy'", text)
        self.assertRegex(text, r"3\s+paint")
        self.assertRegex(text, r"1\s+decode")
        self.assertRegex(text, r"1\s+\(no other function on the stack\)")


if __name__ == "__main__":
    unittest.main()
