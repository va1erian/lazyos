#!/usr/bin/env python3
"""Tests for `transfers (...)` clauses in midlc.

Run: python tools/midlc/test_midlc_transfers.py
"""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import midlc  # noqa: E402

SAMPLE = """
interface os.lazy.demo.v1 {
    method Open(id: U64) -> (session: U64) = 1
        transfers (events: Channel<os.lazy.demo.v1>);
    method Attach(slots: U32) -> ()
        transfers (ring: Buffer, notify: Channel<os.lazy.demo.v1>);
    method Plain() -> ();
    method Tick(n: U32) -> () oneway;
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
    def test_slots_follow_declaration_order_per_kind(self) -> None:
        attach = parse(SAMPLE)[0].methods[1]
        self.assertEqual(
            [(t.name, t.kind, t.index, t.interface) for t in attach.transfers],
            [("ring", "buffer", 0, None), ("notify", "channel", 0, "os.lazy.demo.v1")],
        )

    def test_clause_mixes_with_id_and_oneway(self) -> None:
        open_ = parse(SAMPLE)[0].methods[0]
        self.assertEqual(open_.method_id, 1)
        self.assertEqual(open_.transfers[0].name, "events")

    def test_rejections(self) -> None:
        cases = {
            "method X() -> () transfers (a: U64);": "`Channel<interface>`, `Buffer` or `Ring<ring, ...>`",
            "method X() -> () transfers (a: Buffer, b: Buffer);": "at most one buffer",
            "method X() -> () transfers (a: Channel<os.lazy.demo.v1>, b: Channel<os.lazy.demo.v1>);": "at most one channel",
            "method X() -> () transfers (a: Buffer, a: Channel<os.lazy.demo.v1>);": "declared twice",
            "method X() -> () transfers (a: Channel);": "exactly one interface",
            "method X() -> () transfers (a: Buffer<U32>);": "no type parameter",
            "method X() -> () transfers ();": "empty",
            "method X() -> () transfers (a: Buffer) transfers (b: Buffer);": "two `transfers`",
            "method X(h: Handle) -> ();": "cannot travel in a message body",
            "method X(b: Option<Buffer>) -> ();": "cannot travel in a message body",
            "method X() -> () transfers (a: Channel<os.lazy.nope.v1>);": "unknown interface",
        }
        for line, message in cases.items():
            with self.subTest(line=line):
                with self.assertRaises(midlc.MidlError) as caught:
                    parse(method_text(line))
                self.assertIn(message, str(caught.exception))

    def test_channel_interface_must_have_a_oneway_method(self) -> None:
        text = """
        interface os.lazy.quiet.v1 { method Ask() -> (); }
        interface os.lazy.loud.v1 {
            method Watch() -> () transfers (events: Channel<os.lazy.quiet.v1>);
        }
        """
        with self.assertRaises(midlc.MidlError) as caught:
            parse(text)
        self.assertIn("no oneway method", str(caught.exception))

    def test_channel_may_name_an_interface_declared_later(self) -> None:
        text = """
        interface os.lazy.first.v1 {
            method Watch() -> () transfers (events: Channel<os.lazy.second.v1>);
        }
        interface os.lazy.second.v1 { method Event() -> () oneway; }
        """
        self.assertEqual(len(parse(text)), 2)

    def test_generated_names_are_claimed(self) -> None:
        text = method_text("method X() -> () transfers (a: Buffer);\n    struct XTransfers { n: U32 }")
        with self.assertRaises(midlc.MidlError) as caught:
            parse(text)
        self.assertIn("XTransfers", str(caught.exception))


class EmitTests(unittest.TestCase):
    def setUp(self) -> None:
        self.interface = parse(SAMPLE)[0]

    def test_rust_helpers(self) -> None:
        rust = midlc.emit_rust(self.interface)
        self.assertIn("use super::transfers;", rust)
        self.assertIn(
            "pub const ATTACH_TRANSFERS: transfers::Transfers = "
            "transfers::Transfers { handles: 1, buffers: 1 };",
            rust,
        )
        self.assertIn("pub struct AttachTransfers {", rust)
        self.assertIn("pub ring: libmessenger::BufferDesc,", rust)
        self.assertIn("(alloc::vec![value.notify], alloc::vec![value.ring])", rust)
        self.assertIn("(alloc::vec![value.events], Vec::new())", rust)
        self.assertIn("METHOD_OPEN => OPEN_TRANSFERS,", rust)
        self.assertIn("_ => transfers::Transfers::NONE,", rust)
        self.assertNotIn("PlainTransfers", rust)

    def test_interface_without_transfers_still_answers(self) -> None:
        plain = parse("interface os.lazy.plain.v1 { method Ping() -> (); }")[0]
        rust = midlc.emit_rust(plain)
        self.assertIn("pub fn request_transfers(method: u32) -> transfers::Transfers {", rust)
        self.assertIn("transfers::Transfers::NONE", rust)

    def test_runtime_is_in_the_crate(self) -> None:
        self.assertIn("pub mod transfers {", midlc.TRANSFER_SUPPORT)
        self.assertIn("pub fn matches(self, handles: u64, buffers: u64) -> bool", midlc.TRANSFER_SUPPORT)

    def test_kernel_table_lists_only_transferring_requests(self) -> None:
        table = midlc.emit_transfer_table([self.interface])
        self.assertIn("pub static DECLARED_TRANSFERS: &[transfers::TransferDecl] = &[", table)
        self.assertIn(f"interface: {self.interface.id:#x},", table)
        self.assertIn("    // os.lazy.demo.v1.Attach", table)
        self.assertIn("transfers: transfers::Transfers { handles: 1, buffers: 1 },", table)
        self.assertIn("transfers: transfers::Transfers { handles: 1, buffers: 0 },", table)
        self.assertNotIn("demo.v1.Plain", table)
        self.assertNotIn("demo.v1.Tick", table)
        self.assertIn("pub fn declared_transfers(interface: u64, method: u32)", table)
        self.assertIn(".map_or(transfers::Transfers::NONE, |decl| decl.transfers)", table)

    def test_kernel_table_is_sorted(self) -> None:
        text = SAMPLE + "interface os.lazy.alpha.v1 { method Go() -> () = 3 transfers (b: Buffer); }"
        table = midlc.emit_transfer_table(parse(text))
        rows = [line.strip() for line in table.splitlines() if line.strip().startswith("interface: ")]
        ids = [int(row.split()[1].rstrip(","), 16) for row in rows]
        self.assertEqual(ids, sorted(ids))
        self.assertEqual(len(ids), 3)

    def test_runtime_has_the_kernel_gate(self) -> None:
        self.assertIn("pub fn allows(self, handles: usize, buffers: usize) -> bool", midlc.TRANSFER_SUPPORT)
        self.assertIn("pub struct TransferDecl {", midlc.TRANSFER_SUPPORT)

    def test_markdown_lists_slots(self) -> None:
        text = midlc.emit_markdown(self.interface)
        self.assertIn("transfers (events: Channel<os.lazy.demo.v1>)` |", text)
        self.assertIn("## Transfers", text)
        self.assertIn("| Attach | `ring` | `buffers[0]`, a shared buffer |", text)
        self.assertIn(
            "| Attach | `notify` | `handles[0]`, a channel the receiver sends `os.lazy.demo.v1` on |",
            text,
        )

    def test_manifest_rows(self) -> None:
        methods = {m["name"]: m for m in midlc.emit_manifest(self.interface)["methods"]}
        self.assertEqual(
            methods["Attach"]["transfers"],
            [
                {"name": "ring", "kind": "buffer", "slot": 0},
                {"name": "notify", "kind": "channel", "slot": 0, "interface": "os.lazy.demo.v1"},
            ],
        )
        self.assertNotIn("transfers", methods["Plain"])

    def test_schema_rows(self) -> None:
        text = midlc.emit_schema([self.interface])
        self.assertIn("use super::schema::{Enum, Field, Interface, Method, Struct, Topic, Transfer, Ty};", text)
        self.assertIn(
            'transfers: &[Transfer { name: "ring", channel: None }, '
            'Transfer { name: "notify", channel: Some("os.lazy.demo.v1") }],',
            text,
        )
        self.assertIn("transfers: &[],", text)


class RepositoryTests(unittest.TestCase):
    """The checked-in `idl/` declares every transfer the services rely on."""

    def test_known_transferring_methods_declare_them(self) -> None:
        root = Path(__file__).resolve().parents[2]
        interfaces = midlc.parse_all(sorted((root / "idl").glob("*.midl")))
        declared = {
            (i.name, m.name): [(t.kind, t.interface) for t in m.transfers]
            for i in interfaces
            for m in i.methods
            if m.transfers
        }
        display = "os.lazy.display.v1"
        self.assertEqual(
            declared,
            {
                (display, "CreateSurface"): [("channel", display)],
                (display, "Subscribe"): [("channel", display)],
                (display, "AttachBuffer"): [("buffer", None)],
                (display, "AttachBufferSlot"): [("buffer", None)],
                ("os.lazy.input.v1", "Open"): [("channel", "os.lazy.input.v1")],
                ("os.lazy.input.v1", "AttachKeyState"): [("buffer", None)],
                ("os.lazy.input.shell.v1", "Attach"): [("channel", "os.lazy.input.shell.v1")],
                ("os.lazy.net.nic.v1", "AttachRing"): [("rings", None), ("channel", "os.lazy.net.nic.v1")],
                ("os.lazy.audio.v1", "AttachRing"): [("rings", None)],
                ("os.lazy.messenger.topics.v1", "Bell"): [
                    ("channel", "os.lazy.messenger.topics.bell.v1")
                ],
                ("os.lazy.messenger.registry.v1", "Connected"): [
                    ("channel", "os.lazy.messenger.registry.v1")
                ],
            },
        )


if __name__ == "__main__":
    unittest.main()
