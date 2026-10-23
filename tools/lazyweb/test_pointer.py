#!/usr/bin/env python3
"""pointer_bench.py's reader, on a made-up log: webd's paint meter, the kernel's CPU looks by program."""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import pointer_bench  # noqa: E402

PAINT = "WEBD:NOTE:INFO:PAINT:mode=fast:n={n}:needs={n}:px=1:clear=0:wk={wk}:swz=1:post=1:avg=1:max=1:lat=1:wheel=0:mouse={mouse}:w2d=0/0"
LOG = "\n".join([
    "WEBD:NOTE:WEBPROCESS:UP:39",
    "LAZYWEB:LOAD:https://arstechnica.com/",
    "LAZYWEB:NAV:https://arstechnica.com/",
    PAINT.format(n=20, wk=40000, mouse=100),
    "PERF:CPU tgid=39 pct=60 user=58 kernel=2 class=normal demotions=0 comm=fork",
    "PERF:CPU tgid=31 pct=40 user=30 kernel=10 class=normal demotions=0 comm=webd.elf",
    "UI:STALL kind=frame ms=120",
    PAINT.format(n=10, wk=20000, mouse=80),
    "PERF:CPU tgid=32 pct=24 user=20 kernel=4 class=normal demotions=0 comm=lazyweb.elf",
    "LAZYWEB:LOAD:https://arstechnica.com/",
    "LAZYWEB:LOAD:about:history",
    PAINT.format(n=30, wk=60000, mouse=0),
    "LAZYWEB:LOAD:about:downloads",
]) + "\n"


class Bench(unittest.TestCase):
    def test_the_phases_are_the_reload_and_the_settled_page(self) -> None:
        found = pointer_bench.phases(LOG)
        self.assertEqual(sorted(found), ["loading", "settled"])
        self.assertEqual(len(found["loading"]), 7)

    def test_the_numbers_come_from_webds_paint_meter_and_the_kernels_cpu_by_program(self) -> None:
        found = pointer_bench.phases(LOG)
        loading = pointer_bench.measure(found["loading"], pointer_bench.procs.engine_pids(LOG))
        self.assertEqual(loading["paints_per_s"], 15)
        self.assertEqual(loading["mouse_per_s"], 90)
        self.assertEqual(loading["paint_ms_per_s"], 30)
        self.assertEqual((loading["web_cpu"], loading["webd_cpu"], loading["client_cpu"]), (60, 40, 24))
        self.assertEqual(loading["ui_stalls"], 1)
        settled = pointer_bench.measure(found["settled"])
        self.assertEqual((settled["paints_per_s"], settled["web_cpu"], settled["ui_stalls"]), (30, 0, 0))

    def test_the_old_prefix_is_not_the_engines_paint_meter(self) -> None:
        old = LOG.replace("WEBD:NOTE:", "LAZYWEB:")
        loading = pointer_bench.measure(pointer_bench.phases(old)["loading"])
        self.assertEqual(loading["paints_per_s"], 0)

    def test_no_mode_file_is_written_any_more(self) -> None:
        text = " ".join(str(step) for step in pointer_bench.session(10.0))
        self.assertNotIn("pointer-mode", text)
        self.assertIn("mimed::open", text)

    def test_the_table_has_the_columns(self) -> None:
        run = {name: pointer_bench.measure(lines) for name, lines in pointer_bench.phases(LOG).items()}
        table = pointer_bench.table([run])
        for column in pointer_bench.KEYS:
            self.assertIn(column, table)


if __name__ == "__main__":
    unittest.main()
