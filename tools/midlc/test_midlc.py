#!/usr/bin/env python3
"""Tests for midlc (issue #90). Run: python tools/midlc/test_midlc.py."""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import midlc  # noqa: E402

SAMPLE = """
/// Demo.
interface os.lazy.demo.v1 {
    /// Say hello.
    method Hello(name: String) -> (reply: String);
    method Fire(payload: Bytes) -> () oneway;
    struct Point { x: I32, y: I32 }
}
"""


class ParserTests(unittest.TestCase):
    def parse(self, text: str) -> midlc.Interface:
        return midlc.Parser(midlc.lex(text)).parse_interface()

    def test_parses_interface(self) -> None:
        interface = self.parse(SAMPLE)
        self.assertEqual(interface.name, "os.lazy.demo.v1")
        self.assertEqual([m.name for m in interface.methods], ["Hello", "Fire"])
        self.assertTrue(interface.methods[1].oneway)
        self.assertEqual(interface.structs[0].name, "Point")

    def test_interface_hash_is_stable(self) -> None:
        self.assertEqual(self.parse(SAMPLE).id, self.parse(SAMPLE).id)
        other = SAMPLE.replace("os.lazy.demo.v1", "os.lazy.demo.v2")
        self.assertNotEqual(self.parse(SAMPLE).id, self.parse(other).id)

    def test_adding_a_method_does_not_renumber(self) -> None:
        before = {m.name: m.method_id for m in self.parse(SAMPLE).methods}
        extended = SAMPLE + "    method Extra() -> ();\n"
        after = {m.name: m.method_id for m in self.parse(extended).methods}
        for name, method_id in before.items():
            self.assertEqual(after[name], method_id, f"{name} was renumbered")

    def test_explicit_ids_win(self) -> None:
        text = SAMPLE.replace(
            "method Hello(name: String) -> (reply: String);",
            "method Hello(name: String) -> (reply: String) = 7;",
        )
        interface = self.parse(text)
        self.assertEqual(interface.methods[0].method_id, 7)

    def test_duplicate_ids_are_rejected(self) -> None:
        text = SAMPLE.replace(
            "method Hello(name: String) -> (reply: String);",
            "method Hello(name: String) -> (reply: String) = 7;",
        ).replace(
            "method Fire(payload: Bytes) -> () oneway;",
            "method Fire(payload: Bytes) -> () = 7 oneway;",
        )
        with self.assertRaises(midlc.MidlError) as caught:
            self.parse(text)
        self.assertIn("used by", str(caught.exception))

    def test_colliding_codec_names_are_rejected(self) -> None:
        # "Point" and "point" both fold to the same generated `encode_point`.
        bad = SAMPLE.replace(
            "struct Point { x: I32, y: I32 }",
            "struct Point { x: I32, y: I32 }\n    struct point { z: I32 }",
        )
        with self.assertRaises(midlc.MidlError) as caught:
            self.parse(bad)
        self.assertIn("codec name", str(caught.exception))

    def test_oneway_cannot_return(self) -> None:
        bad = SAMPLE.replace("-> () oneway", "-> (x: U32) oneway")
        with self.assertRaises(midlc.MidlError):
            self.parse(bad)

    def test_missing_arrow_is_a_friendly_error(self) -> None:
        with self.assertRaises(midlc.MidlError) as caught:
            self.parse("interface os.lazy.bad.v1 { method Broken(x: U32); }")
        self.assertIn("expected '->'", str(caught.exception))
        self.assertIn("line", str(caught.exception))

    def test_unknown_type_is_rejected(self) -> None:
        with self.assertRaises(midlc.MidlError) as caught:
            self.parse("interface os.lazy.bad.v1 { method M(x: Wibble) -> (); }")
        self.assertIn("unknown type", str(caught.exception))

    def test_bad_interface_name_is_rejected(self) -> None:
        with self.assertRaises(midlc.MidlError):
            self.parse("interface NotVersioned { }")


class CodegenTests(unittest.TestCase):
    def test_generation_is_deterministic(self) -> None:
        interface = midlc.Parser(midlc.lex(SAMPLE)).parse_interface()
        self.assertEqual(midlc.emit_rust(interface), midlc.emit_rust(interface))

    def test_manifest_shape(self) -> None:
        interface = midlc.Parser(midlc.lex(SAMPLE)).parse_interface()
        manifest = midlc.emit_manifest(interface)
        self.assertEqual(manifest["interface"], "os.lazy.demo.v1")
        self.assertTrue(manifest["interface_id"].startswith("0x"))
        self.assertEqual(len(manifest["methods"]), 2)


if __name__ == "__main__":
    unittest.main(verbosity=2)

