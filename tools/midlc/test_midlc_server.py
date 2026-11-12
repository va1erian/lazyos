#!/usr/bin/env python3
"""Tests for the Rust server emitter (`docs/client-api-plan.md` 3.3).

Run: python tools/midlc/test_midlc_server.py
"""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from midlc_rust import emit_rust  # noqa: E402
from midlc_rust_client import emit_client, has_client  # noqa: E402
from midlc_rust_server import emit_server, has_server  # noqa: E402
from midlc_split import features_block  # noqa: E402
from test_midlc_client import DEMO, parse  # noqa: E402


def server_text(name: str, text: str = DEMO) -> str:
    return "\n".join(emit_server(parse(text)[name]))


class ServerTests(unittest.TestCase):
    def test_trait_and_dispatch_cover_every_method(self) -> None:
        text = server_text("os.lazy.demo.v1")
        self.assertIn("pub trait Server {", text)
        self.assertIn("pub fn dispatch<S: Server>(server: &mut S, message: &impl libmessenger::server::Incoming)", text)
        for name in ("get", "slow", "pair", "hand", "offer", "maybe", "fire", "connect"):
            self.assertIn(f"fn {name}(&mut self, cx: &libmessenger::server::Caller", text)
        self.assertIn("METHOD_GET if id == INTERFACE_ID", text)

    def test_replies_follow_the_client_shape(self) -> None:
        text = server_text("os.lazy.demo.v1")
        # One field: the field itself; several: the reply struct; none: unit.
        self.assertIn("fn get(&mut self, cx: &libmessenger::server::Caller, path: alloc::string::String) -> Result<core::option::Option<Item>,", text)
        self.assertIn("fn pair(&mut self, cx: &libmessenger::server::Caller) -> Result<PairReply,", text)
        self.assertIn("fn connect(&mut self, cx: &libmessenger::server::Caller, name: alloc::string::String) -> Result<(),", text)

    def test_objects_are_claimed_by_the_decoder(self) -> None:
        text = server_text("os.lazy.demo.v1")
        self.assertIn("message.decode(decode_hand_args)", text)
        self.assertIn("message.decode(libmessenger::server::no_objects(decode_get_args))", text)

    def test_error_sentences_are_escaped_as_rust_string_literals(self) -> None:
        from midlc_model import ErrorCode

        interface = parse()["os.lazy.demo.v1"]
        interface.errors = [ErrorCode("Odd", 9, 'a \\ b "c"')]
        text = "\n".join(emit_server(interface))
        self.assertIn("ServiceError::Odd => \"a \\\\ b 'c'\"", text)

    def test_a_scoped_method_matches_its_scope_id(self) -> None:
        text = server_text("os.lazy.demo.v1")
        self.assertIn("METHOD_OFFER if id == METHOD_OFFER_SCOPE", text)
        self.assertIn("_ if id == INTERFACE_ID || id == METHOD_OFFER_SCOPE =>", text)

    def test_a_oneway_method_returns_nothing(self) -> None:
        text = server_text("os.lazy.demo.v1")
        self.assertIn("fn fire(&mut self, cx: &libmessenger::server::Caller, code: u32);", text)
        self.assertNotIn("encode_fire_reply", text)

    def test_every_method_is_required(self) -> None:
        text = server_text("os.lazy.demo.v1")
        self.assertNotIn("NOSYS", text.split("pub trait Server")[1].split("#[cfg")[0])
        self.assertIn("fn pair(&mut self, cx: &libmessenger::server::Caller) -> Result<PairReply, libmessenger::server::Fault>;", text)

    def test_declared_errors_become_faults(self) -> None:
        text = server_text("os.lazy.demo.v1")
        self.assertIn("impl From<ServiceError> for libmessenger::server::Fault", text)
        self.assertNotIn("From<ServiceError>", server_text("os.lazy.demo.plain.v1"))

    def test_gated_behind_the_server_feature(self) -> None:
        text = server_text("os.lazy.demo.plain.v1")
        self.assertTrue(text.lstrip().startswith('#[cfg(feature = "demo_plain_v1_server")]'))
        block = features_block(list(parse().values()))
        self.assertIn('demo_v1_server = ["demo_v1"]', block)

    def test_callback_interfaces_get_a_server_but_kernel_scopes_do_not(self) -> None:
        self.assertTrue(has_server(parse()["os.lazy.demo.callback.v1"]))
        interface = parse()["os.lazy.demo.v1"]
        interface.name = "os.lazy.messenger.policy.v1"
        self.assertFalse(has_server(interface))
        interface.name = "os.lazy.messenger.registry.v1"
        self.assertFalse(has_server(interface))

    def test_the_module_carries_the_server(self) -> None:
        self.assertIn("pub fn dispatch<S: Server>", emit_rust(parse()["os.lazy.demo.v1"]))
        self.assertNotIn("pub fn dispatch<S: Server>", emit_rust(parse()["os.lazy.demo.v1"], client=False))


FEED = """
service "os.lazy.feed" implements os.lazy.feed.v1, os.lazy.feed.events.v1;

interface os.lazy.feed.v1 {
    default deadline=1s;
    method Watch(events: Channel<os.lazy.feed.events.v1> = 1) -> () = 1;
    method Poke(code: U32 = 1) -> () = 2 oneway;
    /// Server to client: the value moved.
    method Moved(x: I32 = 1, y: I32 = 2) -> () = 3 event;
}
interface os.lazy.feed.events.v1 {
    method Tick(n: U32 = 1) -> () = 1 event;
}
"""


class EventTests(unittest.TestCase):
    def test_event_is_oneway_on_the_wire_and_flagged(self) -> None:
        interface = parse(FEED)["os.lazy.feed.v1"]
        poke, moved = interface.methods[1], interface.methods[2]
        self.assertTrue(poke.oneway and not poke.event)
        self.assertTrue(moved.oneway and moved.event)
        self.assertEqual([m.name for m in interface.calls], ["Watch", "Poke"])

    def test_sync_cancels_an_earlier_event(self) -> None:
        text = FEED.replace("= 3 event;", "= 3 event sync;")
        moved = parse(text)["os.lazy.feed.v1"].methods[2]
        self.assertFalse(moved.oneway or moved.event)

    def test_the_server_has_no_event_function_or_arm(self) -> None:
        text = server_text("os.lazy.feed.v1", FEED)
        self.assertIn("fn poke(&mut self", text)
        self.assertNotIn("fn moved", text)
        self.assertNotIn("METHOD_MOVED", text)

    def test_an_events_only_interface_gets_no_server_or_client(self) -> None:
        events = parse(FEED)["os.lazy.feed.events.v1"]
        self.assertFalse(has_server(events))
        self.assertFalse(has_client(events))

    def test_the_wire_codecs_of_an_event_stay(self) -> None:
        text = emit_rust(parse(FEED)["os.lazy.feed.v1"])
        self.assertIn("pub const METHOD_MOVED: u32 = 3;", text)
        self.assertIn("pub fn encode_moved_args(", text)
        self.assertIn("pub fn decode_moved_args(", text)

    def test_the_client_cannot_call_an_event(self) -> None:
        text = "\n".join(emit_client(parse(FEED)["os.lazy.feed.v1"]))
        self.assertIn("pub fn poke(", text)
        self.assertNotIn("pub fn moved(", text)

    def test_an_event_still_satisfies_the_channel_rule(self) -> None:
        parse(FEED)  # `Channel<events>` needs a oneway method on the target


if __name__ == "__main__":
    unittest.main()
