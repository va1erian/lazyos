#!/usr/bin/env python3
"""Tests for `ring` declarations and `Ring<...>` transfers in midlc.

Run: python tools/midlc/test_midlc_rings.py
"""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import midlc  # noqa: E402

NIC = """
interface os.lazy.nic.v1 {
    method Attach(slots: U32) -> (ring: U32)
        transfers (rings: Ring<Rx, Tx>, notify: Channel<os.lazy.nic.v1>);
    method Kick(ring: U32) -> () oneway;
    method Notify(ring: U32) -> () oneway;
    method Commit(written: U64) -> (consumed: U64);
    /// Frames in.
    ring Rx : frames producer=server doorbell=Notify;
    ring Tx : frames producer=client doorbell=Kick;
}
"""


def parse(text: str) -> midlc.Interface:
    interfaces = midlc.Parser(midlc.lex(text)).parse_interfaces()
    midlc.check_channel_targets(interfaces)
    return interfaces[0]


class ParseTests(unittest.TestCase):
    def test_rings_and_their_transfer(self) -> None:
        nic = parse(NIC)
        self.assertEqual(
            [(r.name, r.layout, r.producer, r.doorbell, r.advance) for r in nic.rings],
            [("Rx", "frames", "server", "Notify", None), ("Tx", "frames", "client", "Kick", None)],
        )
        self.assertEqual(nic.rings[0].doc, "Frames in.")
        rings = nic.methods[0].transfers[0]
        self.assertEqual((rings.kind, rings.index, rings.rings), ("rings", 0, ["Rx", "Tx"]))

    def test_a_ring_takes_the_buffer_slot(self) -> None:
        text = NIC.replace("Ring<Rx, Tx>, notify", "Ring<Rx, Tx>, extra: Buffer, notify")
        with self.assertRaises(midlc.MidlError) as caught:
            parse(text)
        self.assertIn("at most one buffer", str(caught.exception))

    def test_rejections(self) -> None:
        cases = {
            # Declarations.
            ("ring Tx : frames producer=client doorbell=Kick;", "ring Tx : bytes producer=client doorbell=Kick;"): "layout 'bytes'",
            ("ring Tx : frames producer=client doorbell=Kick;", "ring Tx : frames producer=both doorbell=Kick;"): "producer=client",
            ("ring Tx : frames producer=client doorbell=Kick;", "ring Tx : frames producer=client;"): "doorbell=",
            ("ring Tx : frames producer=client doorbell=Kick;", "ring Tx : frames producer=client doorbell=Kick advance=Commit;"): "no advance=",
            ("ring Tx : frames producer=client doorbell=Kick;", "ring Tx : stream producer=client doorbell=Kick;"): "advance= method",
            ("ring Tx : frames producer=client doorbell=Kick;", "ring Tx : frames producer=client doorbell=Commit;"): "not a oneway method",
            ("ring Tx : frames producer=client doorbell=Kick;", "ring Tx : stream producer=client advance=Nope;"): "not a method",
            ("ring Tx : frames producer=client doorbell=Kick;", "ring Tx : frames producer=client doorbell=Kick colour=red;"): "unknown option",
            ("ring Tx : frames producer=client doorbell=Kick;", "ring Tx : frames producer=client producer=client doorbell=Kick;"): "given twice",
            ("ring Tx : frames producer=client doorbell=Kick;", "ring Tx : frames producer=client doorbell=Kick;\n    ring Tx : frames producer=client doorbell=Kick;"): "declared twice",
            ("ring Tx : frames producer=client doorbell=Kick;", "ring Tx : frames producer=client doorbell=Kick;\n    ring Spare : frames producer=client doorbell=Kick;"): "no method transfers it",
            # Transfers.
            ("Ring<Rx, Tx>", "Ring<Rx, Nope>"): "'Nope' is not a ring",
            ("Ring<Rx, Tx>", "Ring<Rx, Rx>"): "listed twice",
            ("Ring<Rx, Tx>", "Ring"): "lists one or more",
            (", notify: Channel<os.lazy.nic.v1>", ""): "must also transfer a Channel<os.lazy.nic.v1>",
        }
        for (old, new), message in cases.items():
            with self.subTest(new=new):
                text = NIC.replace(old, new)
                self.assertNotEqual(text, NIC)
                with self.assertRaises(midlc.MidlError) as caught:
                    parse(text)
                self.assertIn(message, str(caught.exception))

    def test_client_produced_ring_needs_no_channel(self) -> None:
        text = """
        interface os.lazy.snd.v1 {
            method Attach(stream: U32) -> () transfers (ring: Ring<Samples>);
            method Commit(written: U64) -> (consumed: U64);
            ring Samples : stream producer=client advance=Commit;
        }
        """
        self.assertEqual(parse(text).rings[0].advance, "Commit")


class EmitTests(unittest.TestCase):
    def setUp(self) -> None:
        self.nic = parse(NIC)

    def test_rust(self) -> None:
        rust = midlc.emit_rust(self.nic)
        self.assertIn("use super::rings;", rust)
        self.assertIn("pub const RING_RX: rings::RingDecl = rings::RingDecl {", rust)
        self.assertIn("producer: rings::Side::Server,", rust)
        self.assertIn("doorbell: Some(METHOD_NOTIFY),", rust)
        self.assertIn("pub const ATTACH_RINGS: [rings::RingDecl; 2] = [RING_RX, RING_TX];", rust)
        self.assertIn("pub fn attach_rings(ring_bytes: u64) -> Option<AttachRings> {", rust)
        self.assertIn("tx: rings::offset(1, ring_bytes)?,", rust)
        self.assertIn("total: rings::offset(2, ring_bytes)?,", rust)
        # The ring buffer is the request's buffer slot.
        self.assertIn("transfers::Transfers { handles: 1, buffers: 1 }", rust)
        self.assertIn("pub rings: libmessenger::BufferDesc,", rust)

    def test_runtime(self) -> None:
        self.assertIn("pub mod rings {", midlc.RING_SUPPORT)
        self.assertIn("index.checked_mul(ring_bytes)", midlc.RING_SUPPORT)

    def test_docs_and_manifest(self) -> None:
        text = midlc.emit_markdown(self.nic)
        self.assertIn("transfers (rings: Ring<Rx, Tx>, notify: Channel<os.lazy.nic.v1>)", text)
        self.assertIn("| `Rx` | frames | server | doorbell `Notify` | Frames in. |", text)
        self.assertIn("a shared buffer holding the rings `Rx`, `Tx` back to back", text)
        manifest = midlc.emit_manifest(self.nic)
        self.assertEqual(
            manifest["rings"][0],
            {"name": "Rx", "layout": "frames", "producer": "server", "doorbell": "Notify"},
        )
        attach = manifest["methods"][0]["transfers"][0]
        self.assertEqual(attach, {"name": "rings", "kind": "rings", "slot": 0, "rings": ["Rx", "Tx"]})

    def test_interface_without_rings_has_no_manifest_entry(self) -> None:
        plain = parse("interface os.lazy.plain.v1 { method Ping() -> (); }")
        self.assertNotIn("rings", midlc.emit_manifest(plain))


if __name__ == "__main__":
    unittest.main()
