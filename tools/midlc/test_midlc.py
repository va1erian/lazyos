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

    def test_multiline_doc_comment_is_preserved(self) -> None:
        # Regression: consecutive `///` lines are one comment; dropping all but
        # the last line truncated the generated docs.
        text = """
        /// First line.
        /// Second line.
        interface os.lazy.docs.v1 {
            /// A struct.
            /// With two lines.
            struct Point { x: I32, y: I32 }
        }
        """
        interface = midlc.Parser(midlc.lex(text)).parse_interface()
        self.assertEqual(interface.docs, "First line.\nSecond line.")
        self.assertEqual(interface.structs[0].doc, "A struct.\nWith two lines.")
        rust = midlc.emit_rust(interface)
        self.assertIn("/// A struct.\n    /// With two lines.", rust)

    def test_emit_rust_includes_interface_id(self) -> None:
        interface = midlc.Parser(midlc.lex(SAMPLE)).parse_interface()
        self.assertIn(f"pub const INTERFACE_ID: u64 = {interface.id:#x};", midlc.emit_rust(interface))

    def test_array_and_option_encode_into_nested_encoder(self) -> None:
        # Regression: the element of an Array/Option must be written into the
        # freshly-created `nested` encoder. Writing it into `target` emitted a
        # field at the wrong depth (and could collide with a sibling id), so
        # the encoded parcel could not be decoded.
        text = """
        interface os.lazy.nested.v1 {
            method M(items: Array<String>, note: Option<U32>) -> (reply: Array<String>);
        }
        """
        interface = midlc.Parser(midlc.lex(text)).parse_interface()
        rust = midlc.emit_rust(interface)
        self.assertIn("nested.string(1, item)", rust)
        self.assertIn("target.array(1, &nested)", rust)
        self.assertIn("nested.u32(1, *item)", rust)
        self.assertIn("target.option(2, Some(&nested))", rust)
        # `Option` must resolve to `core`, not `alloc`.
        self.assertIn("core::option::Option<u32>", rust)

    def test_doc_of_empty_method_does_not_leak_to_next_item(self) -> None:
        # A `() -> ()` method emits no argument or reply struct; its doc
        # comment used to be left dangling onto the next method's struct.
        text = """
        interface os.lazy.dangling.v1 {
            /// Probe doc.
            method Ping() -> ();
            /// Set doc.
            method Set(value: U32) -> ();
        }
        """
        interface = midlc.Parser(midlc.lex(text)).parse_interface()
        rust = midlc.emit_rust(interface)
        self.assertNotIn("Probe doc.", rust)
        self.assertIn("/// Set doc.\n    #[derive", rust)


class EnumConstantCollisionTests(unittest.TestCase):
    def test_colliding_enum_constants_are_rejected(self):
        source = """
        interface os.lazy.clash.v1 {
            method A() -> ();
            enum Qos { LevelHigh }
            enum QosLevel { High }
        }
        """
        with self.assertRaises(midlc.MidlError):
            for interface in midlc.Parser(midlc.lex(source)).parse_interfaces():
                midlc.validate(interface)


class MultiInterfaceTests(unittest.TestCase):
    TWO = """
    /// First.
    interface os.lazy.one.v1 {
        method A() -> ();
        enum Mode { Publish, Subscribe }
    }
    /// Second.
    interface os.lazy.one.sub.v1 {
        method A() -> ();
    }
    """

    def test_one_file_may_hold_several_interfaces(self) -> None:
        found = midlc.Parser(midlc.lex(self.TWO)).parse_interfaces()
        self.assertEqual([i.name for i in found], ["os.lazy.one.v1", "os.lazy.one.sub.v1"])
        self.assertEqual([i.docs for i in found], ["First.", "Second."])
        self.assertNotEqual(found[0].id, found[1].id)

    def test_single_interface_file_still_parses(self) -> None:
        found = midlc.Parser(midlc.lex(SAMPLE)).parse_interfaces()
        self.assertEqual(len(found), 1)

    def test_garbage_after_an_interface_is_rejected(self) -> None:
        with self.assertRaises(midlc.MidlError):
            midlc.Parser(midlc.lex(SAMPLE + " struct Stray { x: U32 }")).parse_interfaces()

    def test_enum_variants_become_u32_constants(self) -> None:
        interface = midlc.Parser(midlc.lex(self.TWO)).parse_interfaces()[0]
        rust = midlc.emit_rust(interface)
        self.assertIn("pub const MODE_PUBLISH: u32 = 0;", rust)
        self.assertIn("pub const MODE_SUBSCRIBE: u32 = 1;", rust)

    def test_multiword_enum_names_snake_case_into_constants(self) -> None:
        text = "interface os.lazy.e.v1 { enum QosLevel { BestEffort, Reliable } }"
        interface = midlc.Parser(midlc.lex(text)).parse_interface()
        rust = midlc.emit_rust(interface)
        self.assertIn("pub const QOS_LEVEL_BEST_EFFORT: u32 = 0;", rust)
        self.assertIn("pub const QOS_LEVEL_RELIABLE: u32 = 1;", rust)


if __name__ == "__main__":
    unittest.main(verbosity=2)

