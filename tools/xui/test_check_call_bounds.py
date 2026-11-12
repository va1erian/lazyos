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
            {str(APP / "m.rs"): "let reply = service.call(i, m, body, Deadline::Never);\n"}
        )
        hits = scan(root)
        self.assertEqual(len(hits), 1)
        self.assertIn("Deadline::Never", hits[0].why)

    def test_an_allowlisted_file_may_wait_for_ever(self):
        rel = "xui-app/src/platform/worker.rs"
        root = build({rel: "let reply = service.call(i, m, body, Deadline::Never);\n"})
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
        root = build({other: "let _ = venue.call(Deadline::Never);\n", parent.rstrip("/") + "/n.rs": ""})
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
                    "// `Deadline::Never` needs a worker thread.\n"
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
                    "let reply = service.call(wire::I, m, body, Deadline::ticks(CALL_TICKS));\n"
                    "let reply = service.call_until(&parcel, deadline());\n"
                )
            }
        )
        self.assertEqual(scan(root), [])

    def test_a_raw_kernel_call_needs_an_entry(self):
        root = build({str(APP / "m.rs"): "let n = lazyos_sys::msg::call(h, &req, &mut buf, 5)?;\n"})
        hits = scan(root)
        self.assertEqual(len(hits), 1)
        self.assertIn("raw `msg::call`", hits[0].why)

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


IDL = """\
service "os.lazy.demo" implements os.lazy.demo.v1;

interface os.lazy.demo.v1 {
    default deadline=never;
    method Quick(a: U32 = 1) -> () deadline=500ms;
    method Slow(a: U32 = 1) -> (b: U32 = 1);
    method ListApps() -> (n: U32 = 1);
    method Connect() -> ();
    method Explicit(a: U32 = 1) -> () deadline=never;
    method Notice(a: U32 = 1) -> () oneway;
}
interface os.lazy.demo.bounded.v1 {
    default deadline=5s;
    method Fine(a: U32 = 1) -> ();
    method Stuck(a: U32 = 1) -> () deadline=never;
}
"""
IMPORTS = "use messenger_generated::os_lazy_demo_v1 as wire;\n"


class IdlDeadlineTests(unittest.TestCase):
    def tree(self, source: str, rel: str = "xui-app/src/platform/m.rs") -> Path:
        return build({"idl/demo.midl": IDL, rel: source})

    def test_a_method_the_idl_leaves_unbounded_by_default_is_reported(self):
        root = self.tree(IMPORTS + "fn f(c: &wire::Client<T>) { c.slow(1); }\n")
        hits = scan(root)
        self.assertEqual(len(hits), 1, hits)
        self.assertIn("`slow` of os_lazy_demo_v1", hits[0].why)
        self.assertIn("deadline=never", hits[0].why)

    def test_an_explicit_never_is_reported_and_a_bounded_default_is_not(self):
        root = self.tree(
            "use messenger_generated::os_lazy_demo_bounded_v1 as wire;\n"
            "fn f(c: &wire::Client<T>) { c.fine(1); c.stuck(1); }\n"
        )
        hits = scan(root)
        self.assertEqual([h.why.split("`")[1] for h in hits], ["stuck"], hits)

    def test_snake_case_and_the_reserved_name_follow_the_generator(self):
        root = self.tree(
            IMPORTS + "fn f(c: &wire::Client<T>) { c.list_apps(); c.connect_call(); }\n"
        )
        self.assertEqual(
            sorted(h.why.split("`")[1] for h in scan(root)), ["connect_call", "list_apps"]
        )

    def test_a_stated_deadline_a_within_call_and_a_oneway_are_not_reported(self):
        root = self.tree(
            IMPORTS
            + "fn f(c: &wire::Client<T>) { c.quick(); c.slow_within(1, d); c.explicit_within(1, d); c.notice(1); }\n"
        )
        self.assertEqual(scan(root), [])

    def test_a_file_that_builds_no_client_of_the_interface_is_not_checked(self):
        root = self.tree(IMPORTS + "fn f(x: &Other) { x.slow(1); }\n")
        self.assertEqual(scan(root), [])

    def test_a_test_file_is_not_checked(self):
        root = self.tree(
            IMPORTS + "fn f(c: &wire::Client<T>) { c.slow(1); }\n",
            "xui-app/crates/x/tests/main.rs",
        )
        self.assertEqual(scan(root), [])

    def test_lazyrad_os_is_scanned_too(self):
        root = self.tree(
            IMPORTS + "fn f(c: &wire::Client<T>) { c.slow(1); }\n", "lazyrad-os/src/m.rs"
        )
        self.assertEqual(len(scan(root)), 1)

    def test_the_real_tree_calls_no_method_the_idl_never_bounds(self):
        self.assertEqual(scan(Path(__file__).resolve().parents[2]), [])


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
