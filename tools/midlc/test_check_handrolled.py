#!/usr/bin/env python3
"""Self-test of check_handrolled.py: patterns, exclusions and the ratchet."""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
import check_handrolled as lint  # noqa: E402


def names(text: str) -> list[str]:
    return [name for _, name in lint.scan_text(text)]


class PatternTests(unittest.TestCase):
    def test_header_literal_only_for_messenger(self) -> None:
        self.assertEqual(names("Header {\n  version: 1,\n}"), ["header"])
        self.assertEqual(names("Header { version: 1 }"), ["header"])
        self.assertEqual(names("Header { magic: 0 }"), [])

    def test_error_field(self) -> None:
        self.assertEqual(names("if f.kind == Kind::Error {} ERROR_FIELD"), ["error-field"] * 2)

    def test_arms(self) -> None:
        self.assertEqual(names("METHOD_A => 1,\nx::METHOD_B | METHOD_C => 2,"), ["arm", "arm"])
        self.assertEqual(names("METHOD_A if x > 1 => 1,"), ["arm"])
        self.assertEqual(names("const METHOD_A: u32 = 1;"), [])

    def test_service_name_and_helpers(self) -> None:
        self.assertEqual(names('const S: &str = "os.lazy.foo.v1";'), ["service-name"])
        self.assertEqual(names('const S: &str = "other";'), [])
        self.assertEqual(names("const fn fnv1a64(t: &str) -> u64 {"), ["fnv1a64"])
        self.assertEqual(names("fn error_reply(m: u32) {}"), ["reply-helper"])
        self.assertEqual(names("fn parcel(method: u32, body: Vec<u8>) {}"), ["reply-helper"])
        self.assertEqual(names("fn parcel(&self) {}"), [])


class ExclusionTests(unittest.TestCase):
    def test_paths(self) -> None:
        for path in (
            "libs/generated/src/lib.rs",
            "libs/messenger/src/server.rs",
            "user/src/messenger/client.rs",
            "xui-app/src/platform/messenger.rs",
            "kernel/src/ipc/connect.rs",
            "user/src/bin/x/tests.rs",
            "user/src/bin/x/object_tests.rs",
            "libs/foo/tests/a.rs",
        ):
            self.assertTrue(lint.is_excluded(path), path)
        self.assertFalse(lint.is_excluded("user/src/bin/netd/sock.rs"))

    def test_production_text(self) -> None:
        self.assertIsNone(lint.production_text("// @generated\nHeader { version: 1 }"))
        text = "fn a() {}\n#[cfg(test)]\nmod t { METHOD_A => 1 }"
        self.assertEqual(lint.production_text(text), "fn a() {}\n")


class RatchetTests(unittest.TestCase):
    def test_ratchet(self) -> None:
        self.assertEqual(lint.ratchet({"a": 2}, {"a": 2}), [])
        self.assertIn("baseline 0", lint.ratchet({"new": 1}, {})[0])
        self.assertIn("baseline 1", lint.ratchet({"a": 2}, {"a": 1})[0])
        self.assertIn("--update-baseline", lint.ratchet({"a": 1}, {"a": 2})[0])
        self.assertIn("--update-baseline", lint.ratchet({}, {"a": 2})[0])

    def test_update_only_lowers(self) -> None:
        self.assertEqual(lint.lowered({"a": 1, "b": 5, "new": 3}, {"a": 2, "b": 4, "gone": 1}), {"a": 1, "b": 4})


if __name__ == "__main__":
    unittest.main()
