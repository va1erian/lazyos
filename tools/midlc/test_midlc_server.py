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
        self.assertIn("message.decode(|body, _| decode_get_args(body))", text)

    def test_a_scoped_method_matches_its_scope_id(self) -> None:
        text = server_text("os.lazy.demo.v1")
        self.assertIn("METHOD_OFFER if id == METHOD_OFFER_SCOPE", text)
        self.assertIn("_ if id == INTERFACE_ID || id == METHOD_OFFER_SCOPE =>", text)

    def test_a_oneway_method_returns_nothing(self) -> None:
        text = server_text("os.lazy.demo.v1")
        self.assertIn("fn fire(&mut self, cx: &libmessenger::server::Caller, code: u32) {", text)
        self.assertNotIn("encode_fire_reply", text)

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


if __name__ == "__main__":
    unittest.main()
