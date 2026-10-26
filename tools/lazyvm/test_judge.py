#!/usr/bin/env python3
"""Self-test of `tools/lazyvm/judge.py` and the result files of `run.py` (no QEMU, no build).

Run: python tools/lazyvm/test_judge.py
"""

from __future__ import annotations

import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))
import boot  # noqa: E402
import judge  # noqa: E402
import run  # noqa: E402

LINE = judge.encode(120, ["/system/lazyvm/bin/lazyvm", "-cp", "a b", ""])


def serial(*lines: str) -> str:
    """A serial log with kernel noise and CR/LF line ends around `lines`."""
    noise = "mem: reclaimed address space 0x2dfc000\r\n"
    return noise + "".join(f"{line}\r\n" for line in lines) + noise


def data(n: int, stream: str, seq: int, payload: bytes) -> str:
    return f"LAZYVM:DATA:{n}:{stream}:{seq}:{payload.hex()}"


class ParseTests(unittest.TestCase):
    def test_the_list_line_hexes_every_argument_empty_ones_too(self) -> None:
        self.assertEqual(LINE.split(" ")[0], "120")
        self.assertEqual(bytes.fromhex(LINE.split(" ")[3]), b"a b")
        self.assertEqual(LINE.split(" ")[4], "")

    def test_output_comes_back_byte_exact_from_chunks_in_order(self) -> None:
        out = b"line one\n\ttab\r\nno newline"
        log = serial("LAZYVM:INIT:1", f"LAZYVM:CMD:1:{LINE.encode().hex()}",
                     data(1, "out", 1, out[10:]), data(1, "out", 0, out[:10]),
                     data(1, "err", 0, b"boom\n"), f"LAZYVM:END:1:1:{len(out)}:5:0:0",
                     "LAZYVM:DONE")
        run_ = judge.parse(log)
        self.assertEqual(run_.errors, [])
        self.assertTrue(run_.done)
        self.assertEqual(run_.count, 1)
        result = run_.results[1]
        self.assertEqual((result.stdout, result.stderr, result.exit), (out, b"boom\n", 1))
        self.assertEqual(result.line, LINE.encode())

    def test_a_lost_chunk_or_a_missing_end_is_an_error(self) -> None:
        lost = judge.parse(serial(data(1, "out", 1, b"x"), "LAZYVM:END:1:0:2:0:0:0"))
        self.assertTrue(any("chunks" in error for error in lost.errors))
        self.assertTrue(any("bytes" in error for error in lost.errors))
        cut = judge.parse(serial(data(1, "out", 0, b"x")))
        self.assertIn("command 1: no END line", cut.errors)

    def test_a_line_cut_by_the_end_of_the_log_is_reported_not_raised(self) -> None:
        parsed = judge.parse("LAZYVM:DATA:1:out:0:6")
        self.assertTrue(parsed.errors)

    def test_timeouts_and_failures_are_read(self) -> None:
        parsed = judge.parse(serial("LAZYVM:END:1:124:0:0:1:1", "LAZYVM:FAIL:cannot read"))
        self.assertTrue(parsed.results[1].timed_out)
        self.assertTrue(parsed.results[1].truncated)
        self.assertEqual(parsed.failure, "cannot read")


class ResultFileTests(unittest.TestCase):
    def setUp(self) -> None:
        self.dir = Path(tempfile.mkdtemp())

    def test_each_command_gets_its_three_files(self) -> None:
        parsed = judge.parse(serial("LAZYVM:INIT:1", f"LAZYVM:CMD:1:{LINE.encode().hex()}",
                                    data(1, "out", 0, b"hi\n"), "LAZYVM:END:1:3:3:0:0:0",
                                    "LAZYVM:DONE"))
        self.assertEqual(run.write_results(parsed, [LINE], self.dir), [])
        self.assertEqual((self.dir / "1.stdout").read_bytes(), b"hi\n")
        self.assertEqual((self.dir / "1.stderr").read_bytes(), b"")
        self.assertEqual((self.dir / "1.exit").read_bytes(), b"3\n")

    def test_an_image_built_for_other_commands_is_refused(self) -> None:
        other = judge.encode(120, ["/system/lazyvm/bin/lazyvm", "Other"])
        parsed = judge.parse(serial("LAZYVM:INIT:1", f"LAZYVM:CMD:1:{other.encode().hex()}",
                                    "LAZYVM:END:1:0:0:0:0:0", "LAZYVM:DONE"))
        problems = run.write_results(parsed, [LINE], self.dir)
        self.assertTrue(any("another one" in problem for problem in problems))
        self.assertFalse((self.dir / "1.exit").exists())

    def test_a_boot_that_never_reached_the_fixture_fails(self) -> None:
        problems = run.write_results(judge.parse("kernel panic\n"), [LINE], self.dir)
        self.assertIn(f"command 1 did not finish: {LINE}", problems)

    def test_the_command_file_splits_like_a_shell(self) -> None:
        path = self.dir / "commands.txt"
        path.write_text("# comment\n\n/x/lazyvm -cp /t \"a b\" ''\n", encoding="utf-8")
        self.assertEqual(run.read_commands(path), [["/x/lazyvm", "-cp", "/t", "a b", ""]])

    def test_an_unbalanced_quote_names_its_line(self) -> None:
        path = self.dir / "commands.txt"
        path.write_text("/x/lazyvm A\n/x/lazyvm 'B\n", encoding="utf-8")
        with self.assertRaisesRegex(ValueError, r"commands.txt:2: .*'B"):
            run.read_commands(path)

    def test_the_timeout_is_at_least_one_second(self) -> None:
        self.assertEqual(run.positive("1"), 1)
        for bad in ("0", "-5"):
            with self.assertRaises(run.argparse.ArgumentTypeError):
                run.positive(bad)

    def test_stale_results_go_and_other_files_stay(self) -> None:
        for name in ("1.stdout", "1.stderr", "12.exit", "notes.txt", "commands.list"):
            (self.dir / name).write_text("old")
        run.clear_results(self.dir)
        self.assertEqual(sorted(p.name for p in self.dir.iterdir()),
                         ["commands.list", "notes.txt"])

    def test_the_build_drops_other_switches_and_keeps_the_stage(self) -> None:
        env = run.build_env({"PATH": "p", "LAZYOS_DESKTOP": "1", "LAZYOS_BUSYBOX": "x",
                             "LAZYOS_LINUXAPPS": "1", "LAZYOS_LAZYVM_STAGE": "s",
                             "LAZYOS_LIMIT_TASK_MAX": "64"}, Path("list"))
        self.assertEqual(env["PATH"], "p")
        for gone in ("LAZYOS_DESKTOP", "LAZYOS_BUSYBOX", "LAZYOS_LINUXAPPS"):
            self.assertNotIn(gone, env)
        self.assertEqual(env["LAZYOS_LAZYVM_STAGE"], "s")
        self.assertEqual(env["LAZYOS_LIMIT_TASK_MAX"], "64")
        self.assertEqual((env["LAZYOS_LAZYVM"], env["LAZYOS_RESET_OS"]), ("1", "1"))
        self.assertEqual(env["LAZYOS_LAZYVM_COMMANDS"], "list")


class DeveloperImageTests(unittest.TestCase):
    """The image a developer has at `target/lazyos.img` survives every run."""

    def setUp(self) -> None:
        self.dir = Path(tempfile.mkdtemp())
        self.built, self.saved = self.dir / "lazyos.img", self.dir / "lazyvm" / "saved.img"

    def test_it_is_set_aside_and_put_back(self) -> None:
        self.built.write_bytes(b"mine")
        self.assertIsNone(run.set_aside(self.built, self.saved))
        self.assertFalse(self.built.exists())
        self.built.write_bytes(b"test build")
        self.assertIsNone(run.put_back(self.built, self.saved))
        self.assertEqual(self.built.read_bytes(), b"mine")
        self.assertFalse(self.saved.exists())

    def test_a_saved_image_left_by_an_earlier_run_is_never_overwritten(self) -> None:
        self.saved.parent.mkdir(parents=True)
        self.saved.write_bytes(b"mine, from the run that died")
        self.built.write_bytes(b"that run's test build")
        refused = run.set_aside(self.built, self.saved)
        self.assertIn(str(self.saved), refused)
        self.assertEqual(self.saved.read_bytes(), b"mine, from the run that died")
        self.assertEqual(self.built.read_bytes(), b"that run's test build")

    def test_a_failed_put_back_says_where_the_image_is(self) -> None:
        self.saved.parent.mkdir(parents=True)
        self.saved.write_bytes(b"mine")
        with mock.patch.object(run.os, "replace", side_effect=PermissionError("in use")):
            left = run.put_back(self.built, self.saved)
        self.assertIn(str(self.saved), left)
        self.assertEqual(self.saved.read_bytes(), b"mine")

    def test_no_image_means_nothing_to_set_aside(self) -> None:
        self.assertIsNone(run.set_aside(self.built, self.saved))
        self.assertIsNone(run.put_back(self.built, self.saved))
        self.assertFalse(self.saved.exists())

    def test_the_build_output_and_the_saved_image_are_refused_as_image(self) -> None:
        self.assertIsNotNone(run.image_refused(run.BUILT))
        self.assertIsNotNone(run.image_refused(run.ROOT / "target" / "." / "lazyos.img"))
        self.assertIsNotNone(run.image_refused(run.SAVED))
        self.assertIsNone(run.image_refused(run.IMAGE))


class WatchTests(unittest.TestCase):
    """When `boot.py` stops waiting: on progress, not on a fixed allowance."""

    def test_a_boot_that_never_reaches_the_fixture_stops(self) -> None:
        watch = boot.Watch(timeout=10, now=0.0, boot=100, idle=30)
        watch.feed("kernel noise\n", 50.0)
        self.assertIsNone(watch.stalled(99.0))
        self.assertIn("did not reach", watch.stalled(101.0))

    def test_a_command_gets_its_timeout_and_streaming_output_keeps_it_alive(self) -> None:
        watch = boot.Watch(timeout=100, now=0.0, boot=100, idle=30)
        watch.feed("LAZYVM:INIT:1\r\nLAZYVM:CMD:1:00\r\n", 1.0)
        # Silent inside its timeout and the fixture's grace: still waiting.
        self.assertIsNone(watch.stalled(1.0 + 100 + boot.GUEST_GRACE + 29))
        self.assertIsNotNone(watch.stalled(1.0 + 100 + boot.GUEST_GRACE + 31))
        # Output long after the timeout (a big stream still draining) keeps it alive.
        watch.feed("LAZYVM:DATA:1:out:0:41\n", 500.0)
        self.assertIsNone(watch.stalled(529.0))
        self.assertIn("no progress", watch.stalled(531.0))

    def test_a_line_split_across_reads_counts_once_whole(self) -> None:
        watch = boot.Watch(timeout=10, now=0.0)
        watch.feed("LAZYVM:IN", 1.0)
        self.assertFalse(watch.started)
        watch.feed("IT:1\nLAZYVM:DONE\n", 2.0)
        self.assertTrue(watch.started)
        self.assertEqual(watch.verdict, "done")

    def test_a_fixture_failure_ends_the_wait(self) -> None:
        watch = boot.Watch(timeout=10, now=0.0)
        watch.feed("LAZYVM:INIT:1\nLAZYVM:FAIL:cannot read\n", 1.0)
        self.assertEqual(watch.verdict, "the fixture failed: cannot read")


if __name__ == "__main__":
    unittest.main()
