#!/usr/bin/env python3
"""Tests for midlc (issue #90). Run: python tools/midlc/test_midlc.py."""

from __future__ import annotations

import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

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
        self.assertIn(f'pub const INTERFACE_NAME: &str = "{interface.name}";', midlc.emit_rust(interface))

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


TOPICS = """
interface os.lazy.demo.v1 {
    method Ping() -> ();
    struct Meta { a: U64, b: String }
    struct Other { c: U64 }
    topic "session/{session}/clipboard/changed" : Meta retained qos=latest;
    topic "system/x/changed/{path...}" : Other qos=conflate;
}
"""


class TopicParserTests(unittest.TestCase):
    def parse(self, text: str) -> midlc.Interface:
        return midlc.Parser(midlc.lex(text)).parse_interface()

    def test_parses_placeholders_and_tail(self) -> None:
        interface = self.parse(TOPICS)
        first, second = interface.topics
        self.assertEqual(first.name, "session/+/clipboard/changed")
        self.assertEqual(first.source, "session/{session}/clipboard/changed")
        self.assertEqual(first.suffix, "session_clipboard_changed")
        self.assertEqual([p.rust_name for p in first.params], ["session"])
        self.assertEqual(first.params[0].kind, "+")
        self.assertEqual(first.payload, "Meta")
        self.assertEqual(first.qos, "latest")
        self.assertTrue(first.retained)
        self.assertEqual(
            first.permissions,
            [
                "publish:session/+/clipboard/changed",
                "subscribe:session/+/clipboard/changed",
            ],
        )
        self.assertEqual(second.name, "system/x/changed/#")
        self.assertEqual(second.suffix, "system_x_changed")
        self.assertEqual(second.params[0].kind, "#")
        self.assertFalse(second.retained)

    def test_slashes_inside_a_string_are_not_a_comment(self) -> None:
        # `//` inside the quoted pattern reaches the parser as content (the
        # empty-segment rule fires), not as a line comment.
        with self.assertRaises(midlc.MidlError) as caught:
            self.parse('interface os.lazy.c.v1 { struct S { a: U64 } topic "a//b" : S; }')
        self.assertIn("empty", str(caught.exception))

    def test_trailing_comment_after_a_topic(self) -> None:
        interface = self.parse(
            'interface os.lazy.c.v1 { struct S { a: U64 } topic "a/b" : S; } // done'
        )
        self.assertEqual(interface.topics[0].name, "a/b")

    def test_punctuation_segments_fold_to_rust_identifiers(self) -> None:
        interface = self.parse('interface os.lazy.p.v1 { struct S { a: U64 } topic "a.b/c-d" : S; }')
        self.assertEqual(interface.topics[0].suffix, "a_b_c_d")

    def test_bare_wildcards_are_accepted(self) -> None:
        interface = self.parse('interface os.lazy.w.v1 { struct S { a: U64 } topic "a/+/b/#" : S; }')
        topic = interface.topics[0]
        self.assertEqual(topic.name, "a/+/b/#")
        self.assertEqual([p.rust_name for p in topic.params], ["wildcard1", "wildcard3"])

    def test_bad_patterns_are_rejected(self) -> None:
        cases = {
            "a//b": "empty",
            "a/#/b": "'#'",
            "/a": "empty",
            "a/": "empty",
            "a/b+c": "mixes",
            "{x}/{x}": "twice",
            "a/b/c/d/e/f/g/h/i": "more than",
            "a/" + "b" * 200: "exceeds",
            "caf\u00e9/x": "refuses",
        }
        for pattern, fragment in cases.items():
            with self.assertRaises(midlc.MidlError, msg=pattern) as caught:
                self.parse(f'interface os.lazy.bad.v1 {{ struct S {{ a: U64 }} topic "{pattern}" : S; }}')
            self.assertIn(fragment, str(caught.exception), pattern)

    def test_placeholder_and_bare_wildcard_name_collision_is_rejected(self) -> None:
        # `{wildcard1}` and the bare `+` at segment 1 both become `wildcard1`.
        with self.assertRaises(midlc.MidlError) as caught:
            self.parse(
                'interface os.lazy.bad.v1 { struct S { a: U64 } topic "{wildcard1}/+" : S; }'
            )
        self.assertIn("Rust name", str(caught.exception))

    def test_rust_keyword_placeholder_is_rejected(self) -> None:
        with self.assertRaises(midlc.MidlError) as caught:
            self.parse('interface os.lazy.bad.v1 { struct S { a: U64 } topic "a/{type}" : S; }')
        self.assertIn("keyword", str(caught.exception))

    def test_unknown_payload_is_rejected(self) -> None:
        with self.assertRaises(midlc.MidlError) as caught:
            self.parse('interface os.lazy.bad.v1 { struct S { a: U64 } topic "a/b" : Nope; }')
        self.assertIn("payload", str(caught.exception))

    def test_bad_qos_is_rejected(self) -> None:
        with self.assertRaises(midlc.MidlError) as caught:
            self.parse('interface os.lazy.bad.v1 { struct S { a: U64 } topic "a/b" : S qos=wild; }')
        self.assertIn("unknown qos", str(caught.exception))

    def test_duplicate_topics_are_rejected(self) -> None:
        text = (
            'interface os.lazy.bad.v1 { struct S { a: U64 } '
            'topic "a/+/b" : S; topic "a/{x}/b" : S; }'
        )
        with self.assertRaises(midlc.MidlError) as caught:
            self.parse(text)
        self.assertIn("declared twice", str(caught.exception))

    def test_topic_colliding_with_a_struct_codec_is_rejected(self) -> None:
        # struct `OfferMeta` generates `encode_offer_meta`; a topic whose
        # literal segments fold to `offer_meta` would generate the same
        # function.
        text = (
            'interface os.lazy.bad.v1 { struct OfferMeta { a: U64 } '
            'topic "offer/meta" : OfferMeta; }'
        )
        with self.assertRaises(midlc.MidlError) as caught:
            self.parse(text)
        self.assertIn("codec name", str(caught.exception))


class TopicCodegenTests(unittest.TestCase):
    def parse(self, text: str) -> midlc.Interface:
        return midlc.Parser(midlc.lex(text)).parse_interface()

    def test_generated_helpers(self) -> None:
        rust = midlc.emit_rust(self.parse(TOPICS))
        self.assertIn(
            'pub const TOPIC_SESSION_CLIPBOARD_CHANGED: &str = "session/+/clipboard/changed";',
            rust,
        )
        self.assertIn("pub fn name_session_clipboard_changed(session: &str)", rust)
        self.assertIn("pub fn publish_session_clipboard_changed<P>", rust)
        self.assertIn("pub fn subscribe_session_clipboard_changed<S>", rust)
        self.assertIn("pub fn encode_session_clipboard_changed(value: &Meta)", rust)
        self.assertIn("pub fn decode_session_clipboard_changed(body: &[u8]) -> Result<Meta, Error>", rust)
        self.assertIn("pub fn name_system_x_changed(path: &str)", rust)
        self.assertIn("topics::build(TOPIC_SYSTEM_X_CHANGED", rust)

    def test_enum_payload_gets_a_u32_codec(self) -> None:
        interface = self.parse('interface os.lazy.e.v1 { enum Level { A, B } topic "x/{n}" : Level; }')
        rust = midlc.emit_rust(interface)
        self.assertIn("pub fn encode_x(value: u32)", rust)
        self.assertIn("pub fn decode_x(body: &[u8]) -> Result<u32, Error>", rust)

    def test_generation_is_deterministic(self) -> None:
        interface = self.parse(TOPICS)
        self.assertEqual(midlc.emit_rust(interface), midlc.emit_rust(interface))

    def test_topic_table_lists_every_interface(self) -> None:
        first = self.parse(TOPICS)
        second = self.parse(
            'interface os.lazy.second.v1 { struct S { a: U64 } topic "y/z" : S; }'
        )
        table = midlc.emit_topic_table([first, second])
        self.assertIn('interface: "os.lazy.demo.v1"', table)
        self.assertIn('interface: "os.lazy.second.v1"', table)
        self.assertIn('publish_permission: "publish:y/z"', table)
        self.assertIn('subscribe_permission: "subscribe:y/z"', table)

    def test_manifest_carries_topics_and_permissions(self) -> None:
        manifest = midlc.emit_manifest(self.parse(TOPICS))
        topics = manifest["topics"]
        self.assertEqual(len(topics), 2)
        self.assertEqual(topics[0]["name"], "session/+/clipboard/changed")
        self.assertEqual(topics[0]["source"], "session/{session}/clipboard/changed")
        self.assertEqual(topics[0]["payload"], "Meta")
        self.assertTrue(topics[0]["retained"])
        self.assertEqual(topics[0]["qos"], "latest")
        self.assertEqual(
            topics[0]["publish_permission"], "publish:session/+/clipboard/changed"
        )
        self.assertEqual(
            topics[0]["subscribe_permission"], "subscribe:session/+/clipboard/changed"
        )
        self.assertFalse(topics[1]["retained"])
        # An interface with no declaration still reports an empty list.
        plain = midlc.emit_manifest(self.parse('interface os.lazy.p.v1 { method M() -> (); }'))
        self.assertEqual(plain["topics"], [])

    def test_markdown_documents_topics(self) -> None:
        markdown = midlc.emit_markdown(self.parse(TOPICS))
        self.assertIn("## Topics", markdown)
        self.assertIn("`session/+/clipboard/changed`", markdown)
        self.assertIn("`Meta`", markdown)
        self.assertIn("`publish:session/+/clipboard/changed`", markdown)


class OutputAtomicityTests(unittest.TestCase):
    def test_parse_error_writes_no_output(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            bad = Path(tmp) / "bad.midl"
            bad.write_text(
                'interface os.lazy.bad.v1 { struct S { a: U64 } topic "a//b" : S; }',
                encoding="utf-8",
            )
            out = Path(tmp) / "out.rs"
            manifest = Path(tmp) / "manifest.json"
            docs = Path(tmp) / "docs"
            argv = [
                "midlc.py",
                "--out",
                str(out),
                "--manifest",
                str(manifest),
                "--docs",
                str(docs),
                str(bad),
            ]
            with mock.patch.object(sys, "argv", argv):
                self.assertEqual(midlc.main(), 1)
            self.assertFalse(out.exists())
            self.assertFalse(manifest.exists())
            self.assertFalse(docs.exists())


class SchemaTests(unittest.TestCase):
    """The `--schema` backend (Rhai `msg` module): data, not codecs."""

    TEXT = """
    /// Demo "quoted" doc.
    interface os.lazy.demo.v1 {
        method Put(items: Array<Point>, tag: Option<String>, level: Level) -> (n: U64);
        method Fire() -> () oneway;
        struct Point { x: I32, y: I32 }
        enum Level { Low, High }
        topic "demo/{who}/moved" : Point retained qos=reliable;
    }
    """

    def schema(self) -> str:
        interfaces = midlc.Parser(midlc.lex(self.TEXT)).parse_interfaces()
        return midlc.emit_schema(interfaces)

    def test_types_resolve_to_structs_enums_and_containers(self) -> None:
        text = self.schema()
        self.assertIn('Field { name: "items", id: 1, ty: Ty::Array(&Ty::Struct("Point")) }', text)
        self.assertIn('Field { name: "tag", id: 2, ty: Ty::Option(&Ty::String) }', text)
        self.assertIn('Field { name: "level", id: 3, ty: Ty::Enum("Level") }', text)
        self.assertIn('variants: &["Low", "High"]', text)

    def test_ids_docs_and_topics_are_carried(self) -> None:
        text = self.schema()
        interface = midlc.Parser(midlc.lex(self.TEXT)).parse_interfaces()[0]
        self.assertIn(f"id: {interface.id:#x},", text)
        self.assertIn(f"id: {interface.methods[0].method_id},", text)
        self.assertIn('doc: "Demo \\"quoted\\" doc."', text)
        self.assertIn('pattern: "demo/+/moved"', text)
        self.assertIn("qos: 3,", text)  # reliable
        self.assertIn("retained: true,", text)
        self.assertIn("oneway: true,", text)
        self.assertIn("params: &[],", text)

    def test_schema_check_reports_staleness(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            source = Path(tmp) / "demo.midl"
            source.write_text(self.TEXT, encoding="utf-8")
            schema = Path(tmp) / "idl.rs"
            write = ["midlc.py", "--schema", str(schema), str(source)]
            check = ["midlc.py", "--check", "--schema", str(schema), str(source)]
            with mock.patch.object(sys, "argv", write):
                self.assertEqual(midlc.main(), 0)
            with mock.patch.object(sys, "argv", check):
                self.assertEqual(midlc.main(), 0)
            schema.write_text("stale", encoding="utf-8")
            with mock.patch.object(sys, "argv", check):
                self.assertEqual(midlc.main(), 1)


if __name__ == "__main__":
    unittest.main(verbosity=2)

