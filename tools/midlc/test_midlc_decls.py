#!/usr/bin/env python3
"""Tests for `service`, `errors`, `scope` and `deadline` declarations in midlc
and for the split crate layout (`docs/client-api-plan.md` 3.1 and 3.6).

Run: python tools/midlc/test_midlc_decls.py
"""

from __future__ import annotations

import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))
import midlc  # noqa: E402
from midlc_decls import attach_services  # noqa: E402
from midlc_model import MidlError, fnv1a64  # noqa: E402
from midlc_split import rewrite_features, split_module  # noqa: E402

DEMO = """
service "os.lazy.demo" implements os.lazy.demo.v1, os.lazy.demo.admin.v1;
service "os.lazy.demo.card/{id}" implements os.lazy.demo.v1;

/// Demo.
interface os.lazy.demo.v1 {
    errors {
        /// Nothing there.
        NotFound = 1001,
        Denied = 1004
    }
    /// Quick.
    method Get(path: String = 1) -> (value: String = 1) deadline=500ms retry=safe;
    method Put(path: String = 1) -> () scope "os.lazy.demo.write.v1" deadline=5s;
    method Fire() -> () oneway;
}
interface os.lazy.demo.admin.v1 {
    default deadline=3s retry=safe;
    method Reset() -> ();
    method Wipe() -> () retry=never deadline=never;
}
"""


def parse_file(text: str) -> list[midlc.Interface]:
    parser = midlc.Parser(midlc.lex(text))
    interfaces = parser.parse_interfaces()
    attach_services(interfaces, parser.services)
    return interfaces


def rejects(text: str) -> str:
    try:
        parse_file(text)
    except MidlError as error:
        return str(error)
    raise AssertionError("expected a MidlError")


class ServiceTests(unittest.TestCase):
    def test_a_service_serves_several_interfaces(self) -> None:
        demo, admin = parse_file(DEMO)
        self.assertEqual(demo.service.name, "os.lazy.demo")
        self.assertEqual(admin.service.name, "os.lazy.demo")
        self.assertEqual(demo.service.interfaces, ["os.lazy.demo.v1", "os.lazy.demo.admin.v1"])

    def test_a_family_keeps_its_prefix_and_placeholder(self) -> None:
        family = parse_file(DEMO)[0].family
        self.assertTrue(family.instanced)
        self.assertEqual((family.prefix, family.placeholder), ("os.lazy.demo.card/", "id"))
        self.assertIsNone(parse_file(DEMO)[1].family)

    def test_unknown_interface_is_rejected(self) -> None:
        text = 'service "os.lazy.x" implements os.lazy.nope.v1;\ninterface os.lazy.x.v1 { method M() -> (); }'
        self.assertIn("unknown interface 'os.lazy.nope.v1'", rejects(text))

    def test_a_service_may_name_an_interface_of_another_file(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            first = Path(tmp) / "a.midl"
            second = Path(tmp) / "b.midl"
            first.write_text('service "os.lazy.a" implements os.lazy.b.v1;\ninterface os.lazy.a.v1 { method M() -> (); }')
            second.write_text("interface os.lazy.b.v1 { method N() -> (); }")
            interfaces = midlc.parse_all([first, second])
        self.assertEqual(interfaces[1].service.name, "os.lazy.a")

    def test_bad_names_and_duplicates_are_rejected(self) -> None:
        body = "interface os.lazy.x.v1 { method M() -> (); }"
        self.assertIn("lowercase", rejects(f'service "Os.Lazy" implements os.lazy.x.v1;\n{body}'))
        self.assertIn("declared twice", rejects(f'service "a.b" implements os.lazy.x.v1;\nservice "a.b" implements os.lazy.x.v1;\n{body}'))
        self.assertIn("twice", rejects(f'service "a.b" implements os.lazy.x.v1, os.lazy.x.v1;\n{body}'))
        self.assertIn("served under both", rejects(f'service "a.b" implements os.lazy.x.v1;\nservice "a.c" implements os.lazy.x.v1;\n{body}'))
        self.assertIn("expected 'implements'", rejects(f'service "a.b" os.lazy.x.v1;\n{body}'))

    def test_services_reach_the_manifest_docs_and_schema(self) -> None:
        demo = parse_file(DEMO)[0]
        self.assertEqual(midlc.emit_manifest(demo)["services"], ["os.lazy.demo", "os.lazy.demo.card/{id}"])
        self.assertIn("Served under the registry name `os.lazy.demo`", midlc.emit_markdown(demo))
        self.assertIn('service: "os.lazy.demo",', midlc.emit_schema([demo]))
        plain = midlc.emit_manifest(parse_file("interface os.lazy.p.v1 { method M() -> (); }")[0])
        self.assertNotIn("services", plain)


class ErrorsTests(unittest.TestCase):
    def test_errors_become_an_enum_with_codes(self) -> None:
        demo = parse_file(DEMO)[0]
        self.assertEqual([(e.name, e.code) for e in demo.errors], [("NotFound", 1001), ("Denied", 1004)])
        rust = midlc.emit_rust(demo)
        self.assertIn("pub enum ServiceError {", rust)
        self.assertIn("NotFound = 1001,", rust)
        self.assertIn("1004 => Some(ServiceError::Denied),", rust)
        self.assertIn("pub const fn code(self) -> u32", rust)
        self.assertIn("/// Nothing there.", rust)

    def test_errors_reach_docs_and_manifest(self) -> None:
        demo = parse_file(DEMO)[0]
        self.assertEqual(midlc.emit_manifest(demo)["errors"][0], {"name": "NotFound", "code": 1001})
        self.assertIn("| 1001 | NotFound | Nothing there. |", midlc.emit_markdown(demo))

    def test_rhai_gets_error_constants(self) -> None:
        source = midlc.emit_rhai_api(parse_file(DEMO))["demo.rhai"]
        self.assertIn("export const ERROR_NOT_FOUND = 1001;", source)
        self.assertIn('export const SERVICE = "os.lazy.demo";', source)

    def wrap(self, errors: str) -> str:
        return f"interface os.lazy.x.v1 {{ errors {{ {errors} }} method M() -> (); }}"

    def test_invalid_blocks_are_rejected(self) -> None:
        self.assertIn("code 0 is reserved", rejects(self.wrap("A = 0")))
        self.assertIn("both have code 7", rejects(self.wrap("A = 7, B = 7")))
        self.assertIn("declared twice", rejects(self.wrap("A = 1, A = 2")))
        self.assertIn("PascalCase", rejects(self.wrap("not_found = 1")))
        self.assertIn("does not fit a u32", rejects(self.wrap("A = 4294967296")))
        self.assertIn("expected a code", rejects(self.wrap("A = x")))
        self.assertIn("one errors block", rejects("interface os.lazy.x.v1 { errors { A = 1 } errors { B = 2 } }"))

    def test_a_struct_may_not_take_the_enum_name(self) -> None:
        text = "interface os.lazy.x.v1 { errors { A = 1 } struct ServiceError { a: U32 } }"
        self.assertIn("collides", rejects(text))


class MethodAttributeTests(unittest.TestCase):
    def test_scope_and_deadline_are_parsed(self) -> None:
        get, put, fire = parse_file(DEMO)[0].methods
        self.assertEqual((get.scope, get.policy.deadline_ms, get.policy.retry), (None, 500, "safe"))
        self.assertEqual((put.scope, put.policy.deadline_ms, put.policy.retry), ("os.lazy.demo.write.v1", 5000, None))
        self.assertEqual((fire.scope, fire.policy.deadline_ms), (None, None))
        self.assertEqual(put.scope_id, fnv1a64("os.lazy.demo.write.v1"))

    def test_constants_are_generated_only_when_declared(self) -> None:
        rust = midlc.emit_rust(parse_file(DEMO)[0])
        self.assertIn("pub const METHOD_GET_DEADLINE: libmessenger::client::Deadline = libmessenger::client::Deadline::millis(500);", rust)
        self.assertIn("pub const METHOD_PUT_DEADLINE: libmessenger::client::Deadline = libmessenger::client::Deadline::millis(5000);", rust)
        self.assertIn("METHOD_GET_RETRY: libmessenger::client::Retry = libmessenger::client::Retry::Safe;", rust)
        self.assertIn("METHOD_PUT_RETRY: libmessenger::client::Retry = libmessenger::client::Retry::Never;", rust)
        self.assertIn(f"pub const METHOD_PUT_SCOPE: u64 = {fnv1a64('os.lazy.demo.write.v1'):#x};", rust)
        self.assertNotIn("METHOD_GET_SCOPE", rust)
        self.assertNotIn("METHOD_FIRE_DEADLINE", rust)
        self.assertNotIn("METHOD_FIRE_RETRY", rust)
        self.assertIn('pub const SERVICE_NAME: &str = "os.lazy.demo";', rust)
        self.assertIn('pub const SERVICE_INSTANCE_PREFIX: &str = "os.lazy.demo.card/";', rust)

    def test_manifest_and_docs_carry_scope_and_deadline(self) -> None:
        demo = parse_file(DEMO)[0]
        put = next(m for m in midlc.emit_manifest(demo)["methods"] if m["name"] == "Put")
        self.assertEqual(put["scope"], "os.lazy.demo.write.v1")
        self.assertEqual(put["scope_id"], f"{fnv1a64('os.lazy.demo.write.v1'):#x}")
        self.assertEqual(put["deadline_ms"], 5000)
        self.assertNotIn("retry", put)
        get = next(m for m in midlc.emit_manifest(demo)["methods"] if m["name"] == "Get")
        self.assertEqual(get["retry"], "safe")
        self.assertIn("| Put | `os.lazy.demo.write.v1` |", midlc.emit_markdown(demo))
        wipe = next(m for m in midlc.emit_manifest(parse_file(DEMO)[1])["methods"] if m["name"] == "Wipe")
        self.assertEqual((wipe["deadline"], wipe["retry"]), ("never", "never"))
        self.assertNotIn("deadline_ms", wipe)

    def wrap(self, attrs: str) -> str:
        return f"interface os.lazy.x.v1 {{ method M() -> () {attrs}; }}"

    def test_invalid_attributes_are_rejected(self) -> None:
        self.assertIn("deadline of 0", rejects(self.wrap("deadline=0ms")))
        self.assertIn("deadline of 0", rejects(self.wrap("deadline=0s")))
        self.assertIn("needs a unit", rejects(self.wrap("deadline=5")))
        self.assertIn("needs a unit", rejects(self.wrap("deadline=5min")))
        self.assertIn("does not fit", rejects(self.wrap("deadline=4294968s")))
        self.assertIn("declares a deadline twice", rejects(self.wrap("deadline=1s deadline=2s")))
        self.assertIn("expected a quoted interface name", rejects(self.wrap("scope os.lazy.a.v1")))
        self.assertIn("reverse-DNS", rejects(self.wrap('scope "Bad Scope"')))
        self.assertIn("declares a scope twice", rejects(self.wrap('scope "a.v1" scope "b.v1"')))
        self.assertIn("no reply to wait for", rejects("interface os.lazy.x.v1 { method M() -> () oneway deadline=1s; }"))
        self.assertIn("no reply to wait for", rejects("interface os.lazy.x.v1 { method M() -> () oneway deadline=never; }"))
        self.assertIn("takes no retry", rejects("interface os.lazy.x.v1 { method M() -> () oneway retry=safe; }"))
        self.assertIn("retry must be one of safe, never", rejects(self.wrap("retry=sometimes")))
        self.assertIn("declares a retry policy twice", rejects(self.wrap("retry=safe retry=never")))
        self.assertIn("declares a deadline twice", rejects(self.wrap("deadline=1s deadline=never")))
        self.assertIn("expected a number or 'never'", rejects(self.wrap("deadline=soon")))

    def test_attributes_combine_with_an_explicit_id(self) -> None:
        text = 'interface os.lazy.x.v1 { method M() -> () = 9 scope "os.lazy.s.v1" deadline=2s retry=safe; }'
        method = parse_file(text)[0].methods[0]
        self.assertEqual((method.method_id, method.policy.deadline_ms, method.policy.retry), (9, 2000, "safe"))

    def test_never_is_a_deadline_and_not_a_number(self) -> None:
        method = parse_file("interface os.lazy.x.v1 { method M() -> () deadline=never; }")[0].methods[0]
        self.assertEqual((method.policy.never, method.policy.deadline_ms), (True, None))


class DefaultPolicyTests(unittest.TestCase):
    def test_the_default_fills_in_what_a_method_leaves_out(self) -> None:
        admin = parse_file(DEMO)[1]
        reset, wipe = admin.methods
        self.assertEqual((reset.policy.deadline_ms, reset.policy.retry), (3000, "safe"))
        self.assertEqual((wipe.policy.never, wipe.policy.retry), (True, "never"))

    def test_a_method_overrides_each_attribute_separately(self) -> None:
        text = "interface os.lazy.x.v1 { default deadline=2s retry=safe; method A() -> () retry=never; method B() -> () deadline=9s; }"
        a, b = parse_file(text)[0].methods
        self.assertEqual((a.policy.deadline_ms, a.policy.retry), (2000, "never"))
        self.assertEqual((b.policy.deadline_ms, b.policy.retry), (9000, "safe"))

    def test_a_oneway_method_takes_nothing_from_the_default(self) -> None:
        text = "interface os.lazy.x.v1 { default deadline=2s retry=safe; method E() -> () oneway; }"
        policy = parse_file(text)[0].methods[0].policy
        self.assertEqual((policy.has_deadline, policy.retry), (False, None))

    def test_an_unstated_retry_resolves_to_never_in_the_generated_code(self) -> None:
        text = 'service "os.lazy.x" implements os.lazy.x.v1;\ninterface os.lazy.x.v1 { default deadline=1s; method M() -> (); }'
        rust = midlc.emit_rust(parse_file(text)[0])
        self.assertIn("METHOD_M_RETRY: libmessenger::client::Retry = libmessenger::client::Retry::Never;", rust)

    def test_invalid_defaults_are_rejected(self) -> None:
        wrap = lambda line: f"interface os.lazy.x.v1 {{ {line} method M() -> (); }}"
        self.assertIn("declares its default policy twice", rejects(wrap("default deadline=1s; default retry=safe;")))
        self.assertIn("states nothing", rejects(wrap("default;")))
        self.assertIn("unexpected 'scope' in `default`", rejects(wrap('default scope "a.v1";')))
        self.assertIn("deadline of 0", rejects(wrap("default deadline=0s;")))
        self.assertIn("retry must be one of", rejects(wrap("default retry=maybe;")))
        self.assertIn("declares a deadline twice", rejects(wrap("default deadline=1s deadline=2s;")))

    def test_a_client_needs_every_two_way_method_to_resolve_a_deadline(self) -> None:
        text = 'service "os.lazy.x" implements os.lazy.x.v1;\ninterface os.lazy.x.v1 { method M() -> (); }'
        with self.assertRaisesRegex(MidlError, "no deadline"):
            midlc.emit_rust(parse_file(text)[0])
        # Without a service there is no client, so nothing is required.
        midlc.emit_rust(parse_file("interface os.lazy.x.v1 { method M() -> (); }")[0])
        # A oneway method needs none.
        midlc.emit_rust(parse_file('service "os.lazy.x" implements os.lazy.x.v1;\ninterface os.lazy.x.v1 { method E() -> () oneway; }')[0])


class SplitLayoutTests(unittest.TestCase):
    def test_a_module_becomes_a_dedented_file_body(self) -> None:
        name, prefix, body = split_module("/// Docs.\n#[rustfmt::skip]\npub mod demo {\n    pub const A: u8 = 1;\n\n    fn f() {\n        ()\n    }\n}\n")
        self.assertEqual(name, "demo")
        self.assertEqual(prefix, ["/// Docs.", "#[rustfmt::skip]"])
        self.assertEqual(body, ["pub const A: u8 = 1;", "", "fn f() {", "    ()", "}"])

    def test_generate_writes_one_file_per_interface_behind_a_feature(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            source = Path(tmp) / "demo.midl"
            source.write_text(DEMO, encoding="utf-8")
            files, _, _ = midlc.generate([source])
        self.assertIn("os_lazy_demo_v1.rs", files)
        self.assertIn("os_lazy_demo_admin_v1.rs", files)
        self.assertIn('#[cfg(feature = "demo_v1")]', files["lib.rs"])
        self.assertIn("pub mod os_lazy_demo_v1;", files["lib.rs"])
        self.assertTrue(files["os_lazy_demo_v1.rs"].startswith("// @generated by tools/midlc/midlc.py"))
        self.assertIn("pub static DECLARED_SERVICES", files["declared.rs"])
        for shared in ("errors.rs", "topics.rs", "objects.rs", "rings.rs", "services.rs"):
            self.assertIn(shared, files)

    def test_feature_block_is_rewritten_between_markers(self) -> None:
        interfaces = parse_file(DEMO)
        cargo = "[features]\n# @generated features begin (tools/midlc/midlc.py, do not edit)\nold\n# @generated features end\n[x]\n"
        text = rewrite_features(cargo, interfaces)
        self.assertIn('default = ["all"]', text)
        self.assertIn('all = ["demo_v1", "demo_admin_v1", "demo_v1_client", "demo_admin_v1_client", "demo_v1_server", "demo_admin_v1_server"]', text)
        self.assertIn("demo_v1 = []", text)
        self.assertIn('demo_v1_server = ["demo_v1"]', text)
        self.assertNotIn("old", text)
        self.assertTrue(text.endswith("[x]\n"))
        self.assertIsNone(rewrite_features("[features]\n", interfaces))

    def test_check_flags_a_stale_module_file_and_passes_when_current(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            source = Path(tmp) / "demo.midl"
            source.write_text(DEMO, encoding="utf-8")
            out = Path(tmp) / "src" / "lib.rs"
            run = lambda *flags: mock.patch.object(sys, "argv", ["midlc.py", *flags, "--out", str(out), str(source)])
            with run():
                self.assertEqual(midlc.main(), 0)
            with run("--check"):
                self.assertEqual(midlc.main(), 0)
            (out.parent / "os_lazy_gone_v1.rs").write_text("// @generated by tools/midlc/midlc.py - do not edit.\n")
            with run("--check"):
                self.assertEqual(midlc.main(), 1)
            with run():
                self.assertEqual(midlc.main(), 0)
            self.assertFalse((out.parent / "os_lazy_gone_v1.rs").exists())


if __name__ == "__main__":
    unittest.main(verbosity=2)
