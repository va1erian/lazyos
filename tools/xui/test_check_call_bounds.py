#!/usr/bin/env python3
"""The scan of `check_call_bounds.py` against trees instead of the repo."""

import tempfile
import unittest
import unittest.mock
from pathlib import Path

from check_call_bounds import ALLOWLIST, Hit, scan, strip_comments

APP = Path("xui-app/src/platform")


def build(files: dict[str, str]) -> Path:
    root = Path(tempfile.mkdtemp())
    for rel, text in files.items():
        path = root / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")
    return root


class ScanTests(unittest.TestCase):
    def test_an_unbounded_call_without_an_entry_is_reported(self):
        root = build(
            {str(APP / "m.rs"): "let reply = service.call_unbounded(i, m, E, body);\n"}
        )
        hits = scan(root)
        self.assertEqual(len(hits), 1)
        self.assertIn("call_unbounded", hits[0].why)

    def test_an_allowlisted_file_may_call_unbounded(self):
        rel = "xui-app/src/platform/worker.rs"
        root = build({rel: "let reply = service.call_detailed_unbounded(i, m, E, body);\n"})
        real = dict(ALLOWLIST)
        ALLOWLIST.clear()
        ALLOWLIST[rel] = "the Files paste worker; a test pins the thread"
        try:
            self.assertEqual(scan(root), [])
        finally:
            ALLOWLIST.clear()
            ALLOWLIST.update(real)

    def test_a_subdirectory_is_covered_by_a_parent_entry_but_not_the_reverse(self):
        parent = "xui-app/src/platform/worker/"
        other = "xui-app/src/platform/api.rs"
        root = build({other: "let _ = venue.call_unbounded();\n", parent.rstrip("/") + "/n.rs": ""})
        real = dict(ALLOWLIST)
        try:
            ALLOWLIST.clear()
            ALLOWLIST[parent] = "tests"
            hits = scan(root)
            self.assertEqual(len(hits), 1, hits)
            self.assertEqual(hits[0].path, other)
            ALLOWLIST.clear()
            ALLOWLIST[other] = "this file alone"
            self.assertEqual(scan(root), [])
        finally:
            ALLOWLIST.clear()
            ALLOWLIST.update(real)

    def test_a_comment_or_doc_never_counts(self):
        root = build(
            {
                str(APP / "m.rs"): (
                    "// `call_unbounded` needs a worker thread.\n"
                    "/* fake: service.call_until(x, 0)\n multi line */\n"
                    "fn bound() -> Parcel { service.call_until(x, deadline(x)) }\n"
                )
            }
        )
        self.assertEqual(scan(root), [])

    def test_a_deadline_less_literal_is_reported(self):
        for body in [
            "fn call() -> Parcel { client.call_until(&parcel, 0) }",
            "fn zero() -> Parcel { sys.msg_call(h, &parcel, &mut buf, 0) }",
        ]:
            root = build({str(APP / "m.rs"): body + "\n"})
            hits = scan(root)
            self.assertEqual(len(hits), 1, body)
            self.assertIn("literal", hits[0].why)

    def test_an_bounded_call_is_not_reported(self):
        root = build(
            {
                str(APP / "m.rs"): (
                    "let reply = service.call(wire::I, m, E, body, CALL_TICKS);\n"
                    "let reply = service.call_until(&parcel, deadline());\n"
                )
            }
        )
        self.assertEqual(scan(root), [])

    def test_a_stale_allowlist_entry_fails_the_run(self):
        root = build({str(APP / "m.rs"): "fn main() {}\n"})
        real = dict(ALLOWLIST)
        ALLOWLIST.clear()
        ALLOWLIST["xui-app/src/gone.rs"] = "moved"
        try:
            with unittest.mock.patch(
                "sys.argv", ["check_call_bounds.py", "--root", str(root)]
            ):
                self.assertEqual(
                    __import__("check_call_bounds").main(),
                    1,
                    "a stale entry is an error",
                )
        finally:
            ALLOWLIST.clear()
            ALLOWLIST.update(real)


class StripTests(unittest.TestCase):
    def test_a_line_comment_ends_at_the_marker(self):
        self.assertEqual(strip_comments(["a // // line"]), ["a "])

    def test_a_block_comment_spans_lines(self):
        self.assertEqual(
            strip_comments(["b /*x*/ y /*", "z*/ tail"]),
            ["b  y ", " tail"],
        )


if __name__ == "__main__":
    unittest.main()
