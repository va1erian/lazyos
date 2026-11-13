#!/usr/bin/env python3
"""Tests for the Java backend (`midlc.py --java`, LazyVM's docs/msg.md).

These check the shape of what is generated; LazyVM compiles it against its
`lazy.msg` runtime and runs it (its `tests/java/lazy/msg`, with the corpus test
`conformance_java.py` writes).

Run: python tools/midlc/test_midlc_java.py
"""

from __future__ import annotations

import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from midlc_decls import attach_services  # noqa: E402
from midlc_java import generate_java  # noqa: E402
from midlc_java_names import base_name, lower_camel, member  # noqa: E402
from midlc_lexer import lex  # noqa: E402
from midlc_model import MidlError  # noqa: E402
from midlc_parser import Parser  # noqa: E402

HERE = Path(__file__).resolve().parent

DEMO = """
service "os.lazy.demo" implements os.lazy.demo.v1;
service "os.lazy.demo.card/{id}" implements os.lazy.demo.card.v1;

interface os.lazy.demo.v1 {
    default deadline=5s;
    errors {
        /// Nothing there.
        NotFound = 1001,
        Denied = 1004
    }
    struct Item { name: String = 1, wait: U32 = 2 }
    enum Mode { Off, OnAir }
    method Get(path: String = 1) -> (value: Option<Item> = 1) deadline=500ms retry=safe;
    method Pair() -> (a: U32 = 1, b: String = 2);
    method Hand(data: Buffer = 1, ch: Channel<os.lazy.demo.plain.v1> = 2) -> (id: U64 = 1);
    method Offer(owner: String = 1) -> () scope "os.lazy.demo.write.v1";
    method Five(a: I32 = 1, b: I32 = 2, c: I32 = 3, d: I32 = 4, e: Option<I64> = 5) -> ();
    method Fire(code: U32 = 1) -> () oneway;
    method Connect(name: String = 1) -> ();
    method Wait(ms: U64 = 1) -> () deadline=never;
    method Attach(slots: U32 = 1, rings: Ring<Rx> = 2) -> (ring: U32 = 1);
    method Notify(ring: U32 = 1) -> () oneway;
    ring Rx : frames producer=client doorbell=Notify;
    topic "demo/{room}/said" : Item qos=buffered;
    topic "demo/mode" : Mode retained qos=latest;
}
interface os.lazy.demo.plain.v1 {
    method Ping() -> () oneway;
}
interface os.lazy.demo.card.v1 {
    default deadline=1s;
    method Poke() -> ();
    method Lit(on: Bool = 1) -> () event;
}
interface os.lazy.demo.callback.v1 {
    method Event() -> () oneway;
}
"""


def generate(text: str) -> dict[str, str]:
    parser = Parser(lex(text))
    interfaces = parser.parse_interfaces()
    attach_services(interfaces, parser.services)
    return generate_java(interfaces)


class JavaBackendTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.files = generate(DEMO)

    def file(self, name: str) -> str:
        return self.files[name]

    def test_files_per_interface(self):
        demo = {p.split("/")[-1] for p in self.files if p.startswith("os/lazy/demo/v1/")}
        self.assertEqual(demo, {
            "Demo.java", "DemoServer.java", "DemoCodec.java", "DemoError.java", "DemoException.java",
            "DemoTopics.java", "Item.java", "Mode.java", "PairReply.java", "FiveArgs.java",
        })
        # A Channel carries plain.v1, and card.v1 has an event: both get a listener.
        self.assertIn("os/lazy/demo/plain/v1/PlainListener.java", self.files)
        self.assertIn("os/lazy/demo/card/v1/CardListener.java", self.files)
        # callback.v1 has a oneway but nothing carries it and it has no event.
        self.assertNotIn("os/lazy/demo/callback/v1/CallbackListener.java", self.files)

    def test_client_names_and_policy(self):
        client = self.file("os/lazy/demo/v1/Demo.java")
        self.assertIn('public static final String SERVICE = "os.lazy.demo";', client)
        self.assertIn("public static Demo connect() {", client)
        self.assertIn("public static Demo connect(String name) {", client)
        # Get: its own deadline and retry=safe; Pair: the interface default.
        self.assertIn("500L, true, false", client)
        self.assertIn("5000L, false, false", client)
        self.assertIn("lazy.internal.msg.Client.NEVER, false, false", client)
        # A method named like a generated member gets a trailing `_`.
        self.assertIn("public CompletionStage<Void> connect_(String name)", client)
        self.assertIn("public CompletionStage<Void> wait_(long ms)", client)
        # Five parameters become a record; a oneway returns a stage of Void.
        self.assertIn("public CompletionStage<Void> five(FiveArgs args)", client)
        self.assertIn("public CompletionStage<Void> fire(long code)", client)
        self.assertIn("public CompletionStage<PairReply> pair()", client)
        self.assertIn("public CompletionStage<Item> get(String path)", client)

    def test_scope_header(self):
        from midlc_model import fnv1a64
        client = self.file("os/lazy/demo/v1/Demo.java")
        self.assertIn(f"0x{fnv1a64('os.lazy.demo.write.v1'):016x}L, DemoCodec.M_OFFER", client)

    def test_family_client(self):
        card = self.file("os/lazy/demo/card/v1/Card.java")
        self.assertIn('SERVICE_PREFIX = "os.lazy.demo.card/";', card)
        self.assertIn("public static Card connectInstance(String instance)", card)
        self.assertNotIn("public static Card connect() {", card)
        # The event is no call: not on the client or the server.
        self.assertNotIn("lit(", card)
        self.assertNotIn("lit(", self.file("os/lazy/demo/card/v1/CardServer.java"))

    def test_rings_left_out(self):
        client = self.file("os/lazy/demo/v1/Demo.java")
        self.assertNotIn("attach(", client)
        self.assertIn("notify(long ring)", client)

    def test_listener_shapes(self):
        lone = self.file("os/lazy/demo/plain/v1/PlainListener.java")
        self.assertIn("@FunctionalInterface", lone)
        self.assertIn("    void ping();", lone)
        server = self.file("os/lazy/demo/v1/DemoServer.java")
        self.assertIn("hand(lazy.msg.Buffer data, os.lazy.demo.plain.v1.PlainListener ch)", server)

    def test_errors_and_topics(self):
        errors = self.file("os/lazy/demo/v1/DemoError.java")
        self.assertIn('NOT_FOUND(1001, "NotFound", "Nothing there.")', errors)
        self.assertIn('DENIED(1004, "Denied", "Denied")', errors)
        topics = self.file("os/lazy/demo/v1/DemoTopics.java")
        self.assertIn("lazy.msg.Topic<Item> demoSaid(String room)", topics)
        self.assertIn("lazy.msg.TopicFilter<Item> demoSaidFilter(String room)", topics)
        self.assertIn("lazy.msg.Topic<Mode> demoMode()", topics)
        self.assertNotIn("demoModeFilter", topics)
        self.assertIn("lazy.msg.Qos.LATEST, true", topics)

    def test_enum_and_record(self):
        self.assertIn("    OFF, ON_AIR", self.file("os/lazy/demo/v1/Mode.java"))
        item = self.file("os/lazy/demo/v1/Item.java")
        self.assertIn('public String name = "";', item)
        self.assertIn("public long wait = 0L;", item)
        self.assertIn("public Item wait_(long wait) {", item)

    def test_names(self):
        self.assertEqual(base_name("os.lazy.net.nic.v1"), "Nic")
        self.assertEqual(base_name("os.lazy.audio_mixer.v1"), "AudioMixer")
        self.assertEqual(lower_camel("replaces_id"), "replacesId")
        self.assertEqual(lower_camel("SetZone"), "setZone")
        self.assertEqual(member("Default", 0), "default_")
        self.assertEqual(member("HashCode", 0), "hashCode_")
        self.assertEqual(member("Equals", 2), "equals")

    def test_taken_type_name(self):
        with self.assertRaises(MidlError):
            generate("interface os.lazy.x.v1 { struct XCodec { a: U32 = 1 } method M() -> (); }")
        with self.assertRaises(MidlError):
            generate("interface os.lazy.x.v1 { struct WireReader { a: U32 = 1 } method M() -> (); }")

    def test_review_fixes(self):
        # A field named like Object.wait(long) keeps its name; its setter does not.
        files = generate("interface os.lazy.x.v1 { struct S { wait: I64 = 1 } method M(s: S = 1) -> (); }")
        record = files["os/lazy/x/v1/S.java"]
        self.assertIn("public long wait = 0L;", record)
        self.assertIn("public S wait_(long wait) {", record)
        # java.lang names, and a Ring inside a struct, are refused with a message.
        for text in ("interface os.lazy.x.v1 { struct Override { a: U32 = 1 } method M() -> (); }",
                     "interface os.lazy.object.v1 { method M() -> (); }"):
            with self.assertRaises(MidlError):
                generate(text)
        # A declared error is only the interface's own when the fault has no domain.
        self.assertIn("f.domain.isEmpty() ? DemoError.of(f.code) : null", self.file("os/lazy/demo/v1/DemoCodec.java"))

    def test_cli_write_and_check(self):
        with tempfile.TemporaryDirectory() as tmp:
            midl = Path(tmp) / "demo.midl"
            midl.write_text(DEMO, encoding="utf-8")
            out = Path(tmp) / "java"
            cmd = [sys.executable, str(HERE / "midlc.py"), "--java", str(out), str(midl)]
            self.assertEqual(subprocess.run(cmd, capture_output=True).returncode, 0)
            self.assertEqual(subprocess.run([*cmd, "--check"], capture_output=True).returncode, 0)
            (out / "os/lazy/demo/v1/Demo.java").write_text("stale", encoding="utf-8")
            self.assertEqual(subprocess.run([*cmd, "--check"], capture_output=True).returncode, 1)

    def test_whole_tree_generates(self):
        files = sorted((HERE.parents[1] / "idl").glob("*.midl"))
        cmd = [sys.executable, str(HERE / "midlc.py"), "--java"]
        with tempfile.TemporaryDirectory() as tmp:
            done = subprocess.run([*cmd, tmp, *map(str, files)], capture_output=True, text=True)
            self.assertEqual(done.returncode, 0, done.stderr)


if __name__ == "__main__":
    unittest.main()
