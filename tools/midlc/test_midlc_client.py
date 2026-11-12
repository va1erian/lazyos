#!/usr/bin/env python3
"""Tests for the Rust client emitter (`docs/client-api-plan.md` 3.2).

Run: python tools/midlc/test_midlc_client.py
"""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import midlc  # noqa: E402
from midlc_decls import attach_services  # noqa: E402
from midlc_model import MidlError  # noqa: E402
from midlc_rust import emit_rust  # noqa: E402
from midlc_rust_client import emit_client, has_client, method_fn, param_type  # noqa: E402
from midlc_split import features_block  # noqa: E402

DEMO = """
service "os.lazy.demo" implements os.lazy.demo.v1, os.lazy.demo.plain.v1;
service "os.lazy.demo.card/{id}" implements os.lazy.demo.card.v1;

interface os.lazy.demo.v1 {
    default deadline=5s;
    errors { NotFound = 1001, Denied = 1004 }
    struct Item { name: String = 1 }
    /// Quick.
    method Get(path: String = 1) -> (value: Option<Item> = 1) deadline=500ms retry=safe;
    method Slow(blob: Bytes = 1, names: Array<String> = 2, item: Item = 3, count: U32 = 4) -> () ;
    method Pair() -> (a: U32 = 1, b: String = 2);
    method Hand(data: Buffer = 1, ch: Channel<os.lazy.demo.plain.v1> = 2) -> (id: U64 = 1);
    method Offer(owner: String = 1) -> () scope "os.lazy.demo.write.v1" deadline=5s;
    method Maybe(tag: Option<String> = 1, raw: Option<Bytes> = 2, item: Option<Item> = 3, n: Option<U32> = 4) -> ();
    method Fire(code: U32 = 1) -> () oneway;
    method Connect(name: String = 1) -> ();
}
interface os.lazy.demo.plain.v1 {
    default deadline=1s retry=safe;
    method Ping() -> ();
}
interface os.lazy.demo.card.v1 {
    default deadline=never;
    method Poke() -> ();
}
interface os.lazy.demo.callback.v1 {
    method Event() -> () oneway;
}
"""


def parse(text: str = DEMO) -> dict[str, midlc.Interface]:
    parser = midlc.Parser(midlc.lex(text))
    interfaces = parser.parse_interfaces()
    attach_services(interfaces, parser.services)
    return {i.name: i for i in interfaces}


def client_text(name: str, text: str = DEMO) -> str:
    return "\n".join(emit_client(parse(text)[name]))


class ClientEmitterTests(unittest.TestCase):
    def setUp(self) -> None:
        self.demo = client_text("os.lazy.demo.v1")

    def test_arguments_are_flattened_and_borrowed(self) -> None:
        self.assertIn("pub fn get(&self, path: &str) -> Result<core::option::Option<Item>, ClientError>", self.demo)
        self.assertIn(
            "pub fn slow(&self, blob: &[u8], names: &[alloc::string::String], item: &Item, count: u32)",
            self.demo,
        )
        self.assertIn("GetArgs { path: path.into() }", self.demo)
        self.assertIn("blob: blob.to_vec(), names: names.to_vec(), item: item.clone(), count", self.demo)

    def test_options_borrow_their_inner_type(self) -> None:
        interface = parse()["os.lazy.demo.v1"]
        maybe = next(m for m in interface.methods if m.name == "Maybe")
        types = [param_type(p.ty) for p in maybe.params]
        self.assertEqual(types[0], ("core::option::Option<&str>", "{}.map(Into::into)"))
        self.assertEqual(types[1], ("core::option::Option<&[u8]>", "{}.map(<[u8]>::to_vec)"))
        self.assertEqual(types[2], ("core::option::Option<&Item>", "{}.cloned()"))
        self.assertEqual(types[3], ("core::option::Option<u32>", "{}"))

    def test_reply_shapes(self) -> None:
        self.assertIn("pub fn slow(&self, blob: &[u8], names: &[alloc::string::String], item: &Item, count: u32) -> Result<(), ClientError>", self.demo)
        self.assertIn("pub fn pair(&self) -> Result<PairReply, ClientError>", self.demo)
        self.assertIn("Ok(reply.value)", self.demo)

    def test_empty_reply_binds_nothing(self) -> None:
        # An unused `wire_reply` would be a warning in every generated file.
        slow = self.demo[self.demo.index("pub fn slow_within") :]
        slow = slow[: slow.index("pub fn pair")]
        self.assertNotIn("wire_reply", slow)

    def test_scope_id_travels_in_the_header(self) -> None:
        self.assertIn(".call(METHOD_OFFER_SCOPE, METHOD_OFFER,", self.demo)
        self.assertIn(".call(INTERFACE_ID, METHOD_GET,", self.demo)

    def test_deadline_is_the_methods_or_the_default(self) -> None:
        self.assertIn("self.get_within(path, METHOD_GET_DEADLINE)", self.demo)
        self.assertIn("self.pair_within(METHOD_PAIR_DEADLINE)", self.demo)
        self.assertIn("at most 500 ms", self.demo)
        self.assertIn("at most 5000 ms", self.demo)
        self.assertNotIn("DEFAULT_DEADLINE", self.demo)

    def test_a_never_deadline_is_stated_in_the_docs(self) -> None:
        card = client_text("os.lazy.demo.card.v1")
        self.assertIn("Waits for the reply for ever", card)
        self.assertIn("self.poke_within(METHOD_POKE_DEADLINE)", card)

    def test_retry_is_passed_to_the_connection(self) -> None:
        self.assertIn("wire_objects, within, METHOD_GET_RETRY)", self.demo)
        self.assertIn("wire_objects, within, METHOD_PAIR_RETRY)", self.demo)
        self.assertIn("(`retry=safe`)", self.demo)
        self.assertIn("only if the request never left (`retry=never`)", self.demo)
        self.assertNotIn("METHOD_FIRE_RETRY", self.demo)

    def test_every_call_has_one_deadline_override(self) -> None:
        for name in ("get", "slow", "pair", "hand", "offer", "maybe"):
            self.assertIn(f"pub fn {name}_within(", self.demo)

    def test_oneway_sends_and_has_no_deadline(self) -> None:
        self.assertIn(".send(INTERFACE_ID, METHOD_FIRE, wire_body, wire_objects)", self.demo)
        self.assertNotIn("fire_within", self.demo)

    def test_objects_move_into_the_parcel(self) -> None:
        self.assertIn("pub fn hand(&self, data: libmessenger::Buffer, ch: u64)", self.demo)
        self.assertIn("let (wire_body, wire_objects) = encode_hand_args(&HandArgs { data, ch })", self.demo)

    def test_errors_map_only_when_declared(self) -> None:
        self.assertIn("pub type ClientError = libmessenger::client::CallError<ServiceError>;", self.demo)
        self.assertIn("error.lift(ServiceError::from_code)", self.demo)
        plain = client_text("os.lazy.demo.plain.v1")
        self.assertIn("CallError<core::convert::Infallible>", plain)
        self.assertNotIn("lift", plain)

    def test_a_method_named_like_a_constructor_is_suffixed(self) -> None:
        method = next(m for m in parse()["os.lazy.demo.v1"].methods if m.name == "Connect")
        self.assertEqual(method_fn(method), "connect_call")
        self.assertIn("pub fn connect_call(&self, name: &str)", self.demo)
        self.assertIn("pub fn connect(transport: T)", self.demo)

    def test_constructors_follow_the_service_declaration(self) -> None:
        self.assertIn("Connection::new(transport, SERVICE_NAME)", self.demo)
        self.assertIn("pub fn try_connect(transport: T)", self.demo)
        family = client_text("os.lazy.demo.card.v1")
        self.assertIn("pub fn from_connection(", family)
        self.assertNotIn("pub fn connect(transport", family)
        self.assertNotIn("SERVICE_NAME", family)

    def test_no_client_without_a_service_or_for_kernel_scopes(self) -> None:
        interfaces = parse()
        self.assertFalse(has_client(interfaces["os.lazy.demo.callback.v1"]))
        self.assertEqual(emit_client(interfaces["os.lazy.demo.callback.v1"]), [])
        scope = parse(DEMO.replace("os.lazy.demo.plain.v1", "os.lazy.messenger.policy.v1"))
        self.assertFalse(has_client(scope["os.lazy.messenger.policy.v1"]))

    def test_everything_is_behind_the_client_feature(self) -> None:
        gates = [line for line in self.demo.splitlines() if line.strip().startswith("#[cfg(")]
        self.assertTrue(gates)
        self.assertTrue(all(line.strip() == '#[cfg(feature = "demo_v1_client")]' for line in gates))
        interface = parse()["os.lazy.demo.v1"]
        self.assertIn("#[cfg(feature", "\n".join(emit_client(interface)))
        self.assertNotIn("pub struct Client", emit_rust(interface, client=False))
        self.assertIn("pub struct Client", emit_rust(interface))

    def test_features_name_each_client(self) -> None:
        block = features_block(list(parse().values()))
        self.assertIn('demo_v1_client = ["demo_v1"]', block)
        self.assertIn('"demo_v1_client"', block.splitlines()[3])
        self.assertNotIn("demo_callback_v1_client", block)

    def test_colliding_names_are_refused(self) -> None:
        clash = DEMO.replace("method Fire(code: U32 = 1) -> () oneway;", "method GetWithin() -> ();")
        with self.assertRaisesRegex(MidlError, "get_within"):
            client_text("os.lazy.demo.v1", clash)
        reserved = DEMO.replace("method Fire(code: U32 = 1)", "method Fire(within: U32 = 1)")
        with self.assertRaisesRegex(MidlError, "within"):
            client_text("os.lazy.demo.v1", reserved)
        with self.assertRaisesRegex(MidlError, "Client"):
            client_text("os.lazy.demo.v1", DEMO.replace("Item", "Client"))


if __name__ == "__main__":
    unittest.main()
