#!/usr/bin/env python3
"""Tests for object parameters (`Channel<I>`, `Buffer`, `Ring<...>`) in midlc.

Run: python tools/midlc/test_midlc_objects.py
"""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import midlc  # noqa: E402

SAMPLE = """
interface os.lazy.demo.v1 {
    method Open(id: U64, events: Channel<os.lazy.demo.v1>) -> (session: U64) = 1;
    method Attach(slots: U32, ring: Buffer, notify: Channel<os.lazy.demo.v1>) -> ();
    method Configure(config: Config) -> ();
    method Plain() -> ();
    method Tick(n: U32) -> () oneway;
    struct Config { name: String, surface: Surface, bell: Channel<os.lazy.demo.v1> }
    struct Surface { width: U32, pixels: Buffer }
    struct Plain { n: U32 }
}
"""


def parse(text: str) -> list[midlc.Interface]:
    interfaces = midlc.Parser(midlc.lex(text)).parse_interfaces()
    midlc.check_channel_targets(interfaces)
    return interfaces


def method_text(method: str) -> str:
    """`SAMPLE` with one extra method line (to probe a rejection)."""
    return SAMPLE.replace("    method Plain() -> ();", f"    method Plain() -> ();\n    {method}")


class ParseTests(unittest.TestCase):
    def test_objects_are_indexed_in_declaration_order(self) -> None:
        attach = parse(SAMPLE)[0].methods[1]
        self.assertEqual(
            [(o.name, o.kind, o.index, o.interface, o.path) for o in attach.objects],
            [("ring", "buffer", 0, None, ["ring"]), ("notify", "channel", 1, "os.lazy.demo.v1", ["notify"])],
        )
        self.assertEqual(attach.params[1].id, 2, "an object is a field with an id")

    def test_objects_inside_structs_are_walked_depth_first(self) -> None:
        configure = parse(SAMPLE)[0].methods[2]
        self.assertEqual(
            [(o.dotted, o.kind, o.index) for o in configure.objects],
            [("config.surface.pixels", "buffer", 0), ("config.bell", "channel", 1)],
        )
        self.assertTrue(configure.objects[0].nested)
        self.assertTrue(configure.params[0].ty.objects)
        plain = parse(SAMPLE)[0].methods[3]
        self.assertEqual(plain.objects, [])

    def test_a_method_mixes_objects_with_an_id(self) -> None:
        open_ = parse(SAMPLE)[0].methods[0]
        self.assertEqual(open_.method_id, 1)
        self.assertEqual(open_.objects[0].name, "events")

    def test_rejections(self) -> None:
        cases = {
            "method X(a: Channel) -> ();": "exactly one interface",
            "method X(a: Buffer<U32>) -> ();": "no type parameter",
            "method X(a: Ring) -> ();": "one or more declared ring names",
            "method X() -> (a: Buffer);": "a reply carries no objects",
            "method X() -> (a: Config);": "a reply carries no objects",
            "method X(a: Option<Buffer>) -> ();": "never sits in an Option or an Array",
            "method X(a: Array<Channel<os.lazy.demo.v1>>) -> ();": "never sits in an Option or an Array",
            "method X(a: Array<Surface>) -> ();": "never sits in an Option or an Array",
            "method X(a: Option<Array<Config>>) -> ();": "never sits in an Option or an Array",
            "method X(h: Handle) -> ();": "`Handle` is not a MIDL type",
            "method X() -> () transfers (a: Buffer);": "`transfers (...)` clause is gone",
            "method X(a: Channel<os.lazy.nope.v1>) -> ();": "unknown interface",
            "method X(a: Buffer, b: Buffer, c: Buffer, d: Buffer, e: Buffer, f: Buffer, g: Buffer, h: Buffer, i: Buffer) -> ();": "at most 8",
            "struct Loop { inner: Loop, b: Buffer }\n    method X(l: Loop) -> ();": "contains itself",
        }
        for line, message in cases.items():
            with self.subTest(line=line):
                with self.assertRaises(midlc.MidlError) as caught:
                    parse(method_text(line))
                self.assertIn(message, str(caught.exception))

    def test_a_topic_payload_carries_no_object(self) -> None:
        text = method_text('topic "demo/surface" : Surface;')
        with self.assertRaises(midlc.MidlError) as caught:
            parse(text)
        self.assertIn("carries a kernel object", str(caught.exception))

    def test_channel_interface_must_have_a_oneway_method(self) -> None:
        text = """
        interface os.lazy.quiet.v1 { method Ask() -> (); }
        interface os.lazy.loud.v1 {
            method Watch(events: Channel<os.lazy.quiet.v1>) -> ();
        }
        """
        with self.assertRaises(midlc.MidlError) as caught:
            parse(text)
        self.assertIn("no oneway method", str(caught.exception))

    def test_channel_may_name_an_interface_declared_later(self) -> None:
        text = """
        interface os.lazy.first.v1 {
            method Watch(events: Channel<os.lazy.second.v1>) -> ();
        }
        interface os.lazy.second.v1 { method Event() -> () oneway; }
        """
        self.assertEqual(len(parse(text)), 2)


class EmitTests(unittest.TestCase):
    def setUp(self) -> None:
        self.interface = parse(SAMPLE)[0]

    def test_rust_request_codecs(self) -> None:
        rust = midlc.emit_rust(self.interface)
        self.assertIn("use super::objects;", rust)
        self.assertIn("pub ring: libmessenger::Buffer,", rust)
        self.assertIn("pub notify: u64,", rust)
        self.assertIn(
            "pub fn encode_attach_args(value: &AttachArgs) -> Result<(Vec<u8>, Vec<libmessenger::Object>), Error> {",
            rust,
        )
        self.assertIn("target.buffer(2, &value.ring, objects)?;", rust)
        self.assertIn("target.channel(3, value.notify, objects)?;", rust)
        self.assertIn(
            "pub fn decode_attach_args(body: &[u8], objects: &[libmessenger::Object]) -> Result<AttachArgs, Error> {",
            rust,
        )
        self.assertIn("out.ring = field.claim_buffer(objects, next)?;", rust)
        self.assertIn("out.notify = field.claim_channel(objects, next)?;", rust)
        self.assertIn("if objects.len() != 2 {", rust)
        self.assertIn("if *next != 2 {", rust)
        self.assertIn(
            "pub const ATTACH_OBJECTS: &[objects::Kind] = &[objects::Kind::Buffer, objects::Kind::Channel];",
            rust,
        )
        self.assertIn("pub const OPEN_OBJECTS: &[objects::Kind] = &[objects::Kind::Channel];", rust)
        self.assertIn("(METHOD_ATTACH, ATTACH_OBJECTS),", rust)
        # A plain method keeps the body-only codec.
        self.assertIn("pub fn encode_tick_args(value: &TickArgs) -> Result<Vec<u8>, Error> {", rust)
        self.assertNotIn("PLAIN_OBJECTS", rust)
        self.assertNotIn("Transfers", rust)

    def test_rust_struct_codecs_thread_the_object_list(self) -> None:
        rust = midlc.emit_rust(self.interface)
        self.assertIn(
            "pub fn encode_surface(value: &Surface, objects: &mut Vec<libmessenger::Object>) -> Result<Vec<u8>, Error> {",
            rust,
        )
        self.assertIn(
            "pub fn decode_surface(body: &[u8], objects: &[libmessenger::Object], next: &mut usize) -> Result<Surface, Error> {",
            rust,
        )
        self.assertIn("target.raw(Kind::Struct, 2, &encode_surface(&value.surface, objects)?)?;", rust)
        self.assertIn("out.surface = decode_surface(field.payload, objects, next)?;", rust)
        self.assertIn("target.raw(Kind::Struct, 1, &encode_config(&value.config, objects)?)?;", rust)
        self.assertIn("out.config = decode_config(field.payload, objects, next)?;", rust)
        # A struct without objects keeps the plain codec.
        self.assertIn("pub fn encode_plain(value: &Plain) -> Result<Vec<u8>, Error> {", rust)
        self.assertIn("pub fn decode_plain(body: &[u8]) -> Result<Plain, Error> {", rust)

    def test_interface_without_objects_still_declares(self) -> None:
        plain = parse("interface os.lazy.plain.v1 { method Ping() -> (); }")[0]
        rust = midlc.emit_rust(plain)
        self.assertIn("pub const DECLARED_OBJECTS: &[(u32, &[objects::Kind])] = &[", rust)

    def test_runtime_is_in_the_crate(self) -> None:
        self.assertIn("pub mod objects {", midlc.OBJECT_SUPPORT)
        self.assertIn("pub use libmessenger::ObjectKind as Kind;", midlc.OBJECT_SUPPORT)
        self.assertIn("pub struct ObjectDecl {", midlc.OBJECT_SUPPORT)

    def test_kernel_table_lists_only_requests_with_objects(self) -> None:
        table = midlc.emit_object_table([self.interface])
        self.assertIn("pub static DECLARED_OBJECTS: &[objects::ObjectDecl] = &[", table)
        self.assertIn(f"interface: {self.interface.id:#x},", table)
        self.assertIn("    // os.lazy.demo.v1.Attach", table)
        self.assertIn("kinds: &[objects::Kind::Buffer, objects::Kind::Channel],", table)
        self.assertIn("kinds: &[objects::Kind::Channel],", table)
        self.assertNotIn("demo.v1.Plain", table)
        self.assertNotIn("demo.v1.Tick", table)
        self.assertIn("pub fn declared_objects(interface: u64, method: u32) -> &'static [objects::Kind] {", table)
        self.assertIn(".map_or(&[], |decl| decl.kinds)", table)

    def test_kernel_table_is_sorted(self) -> None:
        text = SAMPLE + "interface os.lazy.alpha.v1 { method Go(b: Buffer) -> () = 3; }"
        table = midlc.emit_object_table(parse(text))
        rows = [line.strip() for line in table.splitlines() if line.strip().startswith("interface: ")]
        ids = [int(row.split()[1].rstrip(","), 16) for row in rows]
        self.assertEqual(ids, sorted(ids))
        self.assertEqual(len(ids), 4)

    def test_markdown_lists_objects(self) -> None:
        text = midlc.emit_markdown(self.interface)
        self.assertIn("`(id: U64, events: Channel<os.lazy.demo.v1>) -> (session: U64)` |", text)
        self.assertIn("## Objects", text)
        self.assertIn("| Attach | `ring` | `Buffer` | `objects[0]`, a shared buffer |", text)
        self.assertIn(
            "| Attach | `notify` | `Channel<os.lazy.demo.v1>` | `objects[1]`, a channel the receiver sends `os.lazy.demo.v1` on |",
            text,
        )
        self.assertIn("| Configure | `config.surface.pixels` | `Buffer` | `objects[0]`, a shared buffer |", text)
        self.assertNotIn("transfers", text)

    def test_manifest_rows(self) -> None:
        methods = {m["name"]: m for m in midlc.emit_manifest(self.interface)["methods"]}
        self.assertEqual(
            methods["Attach"]["objects"],
            [
                {"name": "ring", "kind": "buffer", "index": 0},
                {"name": "notify", "kind": "channel", "index": 1, "interface": "os.lazy.demo.v1"},
            ],
        )
        self.assertEqual(
            methods["Configure"]["objects"][0],
            {"name": "pixels", "kind": "buffer", "index": 0, "path": ["config", "surface", "pixels"]},
        )
        self.assertNotIn("objects", methods["Plain"])
        self.assertNotIn("transfers", methods["Attach"])

    def test_schema_rows(self) -> None:
        text = midlc.emit_schema([self.interface])
        self.assertIn("use super::schema::{Enum, Field, Interface, Method, Object, Struct, Topic, Ty};", text)
        self.assertIn(
            'objects: &[Object { name: "ring", channel: None }, '
            'Object { name: "notify", channel: Some("os.lazy.demo.v1") }],',
            text,
        )
        self.assertIn('Object { name: "config.surface.pixels", channel: None }', text)
        self.assertIn("objects: &[],", text)
        self.assertIn("ty: Ty::Buffer", text)
        self.assertIn("ty: Ty::Channel", text)


class RepositoryTests(unittest.TestCase):
    """The checked-in `idl/` declares every object the services rely on."""

    def test_known_methods_carry_their_objects(self) -> None:
        root = Path(__file__).resolve().parents[2]
        interfaces = midlc.parse_all(sorted((root / "idl").glob("*.midl")))
        declared = {
            (i.name, m.name): [(o.name, o.kind, o.interface) for o in m.objects]
            for i in interfaces
            for m in i.methods
            if m.objects
        }
        display = "os.lazy.display.v1"
        self.assertEqual(
            declared,
            {
                (display, "CreateSurface"): [("events", "channel", display)],
                (display, "Subscribe"): [("events", "channel", display)],
                (display, "AttachBuffer"): [("pixels", "buffer", None)],
                (display, "AttachBufferSlot"): [("pixels", "buffer", None)],
                ("os.lazy.input.v1", "Open"): [("events", "channel", "os.lazy.input.v1")],
                ("os.lazy.input.v1", "AttachKeyState"): [("state", "buffer", None)],
                ("os.lazy.input.shell.v1", "Attach"): [("events", "channel", "os.lazy.input.shell.v1")],
                ("os.lazy.net.nic.v1", "AttachRing"): [
                    ("rings", "rings", None),
                    ("notify", "channel", "os.lazy.net.nic.v1"),
                ],
                ("os.lazy.audio.v1", "AttachRing"): [("ring", "rings", None)],
                ("os.lazy.messenger.topics.v1", "Bell"): [
                    ("bell", "channel", "os.lazy.messenger.topics.bell.v1")
                ],
                ("os.lazy.messenger.registry.v1", "Connected"): [
                    ("connection", "channel", "os.lazy.messenger.registry.v1")
                ],
                ("os.lazy.shell.tray.v1", "Set"): [("events", "channel", "os.lazy.shell.tray.events.v1")],
                ("os.lazy.init.app.v1", "Watch"): [("events", "channel", "os.lazy.init.app.events.v1")],
            },
        )


if __name__ == "__main__":
    unittest.main()
