#!/usr/bin/env python3
"""Tests for the Rhai API backend (`midlc --rhai-api`, `midlc_rhai`).

Run: python tools/midlc/test_midlc_rhai.py
"""

from __future__ import annotations

import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import midlc  # noqa: E402
import midlc_rhai  # noqa: E402

ROOT = HERE.parent.parent

DEMO = """
/// The demo service.
///
/// Second paragraph.
interface os.lazy.demo.v1 {
    /// Say hello.
    method Hello(name: String, new: U32) -> (reply: String);
    method Fire(payload: Bytes) -> () oneway;
    method Attach(slot: U32) -> () transfers (events: Channel<os.lazy.demo.v1>);
    struct Point { x: I32, y: F64, tag: Option<String>, list: Array<U32>, mode: Mode, inner: Inner }
    struct Inner { on: Bool }
    enum Mode { Fast, VerySlow }
    /// Something moved.
    topic "system/demo/moved/{path...}" : Point qos=buffered;
    topic "system/events/demo/ready" : Inner retained qos=latest;
}
"""


def parse(text: str) -> list[midlc.Interface]:
    return midlc.Parser(midlc.lex(text)).parse_interfaces()


def module(text: str = DEMO) -> str:
    return midlc_rhai.emit_module(parse(text)[0])


class NamingTests(unittest.TestCase):
    def test_aliases_drop_the_vendor_prefix_and_version(self) -> None:
        cases = {
            "os.lazy.confd.v1": "confd",
            "os.lazy.net.nic.v1": "net_nic",
            "os.lazy.messenger.topics.v1": "messenger_topics",
            "org.example.thing.v2": "org_example_thing",
        }
        for name, alias in cases.items():
            self.assertEqual(midlc_rhai.alias(midlc.Interface(name)), alias)

    def test_two_interfaces_with_one_alias_are_refused(self) -> None:
        clash = parse(DEMO) + parse(DEMO.replace("os.lazy.demo.v1", "os.lazy.demo.v2"))
        with self.assertRaisesRegex(midlc.MidlError, "sys::demo"):
            midlc_rhai.scriptable(clash)

    def test_kernel_scopes_get_no_module(self) -> None:
        interfaces = midlc.parse_all(sorted((ROOT / "idl").glob("*.midl")))
        names = {i.name for i in midlc_rhai.scriptable(interfaces)}
        self.assertIn("os.lazy.confd.v1", names)
        self.assertTrue(midlc_rhai.KERNEL_SCOPES.isdisjoint(names))
        self.assertTrue(midlc_rhai.KERNEL_SCOPES <= {i.name for i in interfaces},
                        "a kernel scope was renamed; update KERNEL_SCOPES")

    def test_topic_helpers_drop_noise_and_the_interface_name(self) -> None:
        demo = parse(DEMO)[0]
        stems = [stem for _, stem in midlc_rhai.topic_helpers(demo)]
        self.assertEqual(stems, ["moved", "ready"])
        health = parse("""interface os.lazy.healthd.v1 {
            struct R { ok: Bool }
            topic "system/health/summary" : R qos=latest;
            topic "system/health/{name}" : R qos=latest;
        }""")[0]
        self.assertEqual([s for _, s in midlc_rhai.topic_helpers(health)], ["summary", "health"])

    def test_clashing_topic_stems_fall_back_to_the_full_suffix(self) -> None:
        clash = parse("""interface os.lazy.x.v1 {
            struct R { ok: Bool }
            topic "system/a/changed" : R qos=latest;
            topic "system/b/changed/{n}" : R qos=latest;
            topic "system/c/changed" : R qos=latest;
        }""")[0]
        stems = [s for _, s in midlc_rhai.topic_helpers(clash)]
        self.assertEqual(stems, ["a_changed", "b_changed", "c_changed"])


class ModuleTests(unittest.TestCase):
    def test_methods_call_msg_with_their_idl_names(self) -> None:
        text = module()
        self.assertIn('fn hello(name, new_) {\n    msg::connect("os.lazy.demo.v1").invoke("Hello", [name, new_])', text)
        self.assertIn("/// `Hello(name: String, new: U32) -> (reply: String)`\n///\n/// Say hello.", text)
        self.assertIn("/// One-way: returns `()` once the message is queued.\nfn fire(payload)", text)

    def test_transferring_methods_are_listed_not_generated(self) -> None:
        text = module()
        self.assertNotIn("fn attach(", text)
        self.assertIn("// or ring): Attach.", text)

    def test_structs_get_zero_value_constructors(self) -> None:
        text = module()
        self.assertIn(
            '#{ x: 0, y: 0.0, tag: (), list: [], mode: "Fast", inner: new_inner() }', text
        )
        self.assertIn("fn new_inner() {\n    #{ on: false }", text)

    def test_enums_are_exported_constants(self) -> None:
        text = module()
        self.assertIn('export const MODE = ["Fast", "VerySlow"];', text)
        self.assertIn('export const MODE_VERY_SLOW = "VerySlow";', text)

    def test_topics_get_pattern_builder_and_handlers(self) -> None:
        text = module()
        self.assertIn('export const MOVED_PATTERN = "system/demo/moved/#";', text)
        self.assertIn("fn moved_topic(path) {\n    `system/demo/moved/${path}`", text)
        self.assertIn('fn on_moved(handler) {\n    msg::on("system/demo/moved/#", handler)', text)
        self.assertIn("fn on_moved(path, handler) {\n    msg::on(`system/demo/moved/${path}`, handler)", text)
        self.assertIn("fn publish_moved(path, payload)", text)
        self.assertIn('fn publish_ready(payload) {\n    msg::publish("system/events/demo/ready", payload)', text)
        self.assertNotIn("fn ready_topic(", text)

    def test_docs_go_on_functions_and_plain_comments_on_constants(self) -> None:
        # With Rhai's `metadata` feature a `///` comment must precede a `fn`.
        lines = module().splitlines()
        for index, line in enumerate(lines):
            if line.startswith("///"):
                following = next(l for l in lines[index:] if not l.startswith("///"))
                self.assertTrue(following.startswith("fn "), f"line {index + 1}: {following}")

    def test_a_reserved_function_name_is_refused(self) -> None:
        bad = "interface os.lazy.bad.v1 {\n    method Import() -> ();\n}"
        with self.assertRaisesRegex(midlc.MidlError, "reserved"):
            module(bad)

    def test_overloads_with_the_same_arity_are_refused(self) -> None:
        bad = """interface os.lazy.bad.v1 {
            struct R { ok: Bool }
            method OnThing(handler: U32) -> ();
            topic "system/bad/thing" : R qos=latest;
        }"""
        with self.assertRaisesRegex(midlc.MidlError, "on_thing"):
            module(bad)


class IndexAndCliTests(unittest.TestCase):
    def test_index_names_aliases_sources_and_topic_patterns(self) -> None:
        index = midlc_rhai.emit_index(parse(DEMO))
        self.assertIn('alias: "demo",', index)
        self.assertIn('source: include_str!("demo.rhai"),', index)
        self.assertIn('ApiTopic { helper: "moved", pattern: "system/demo/moved/#" }', index)

    def test_reference_lists_every_function(self) -> None:
        files = midlc_rhai.emit_rhai_api(parse(DEMO))
        self.assertEqual(sorted(files), ["README.md", "demo.rhai", "index.rs"])
        readme = files["README.md"]
        self.assertIn("## `sys::demo`", readme)
        self.assertIn("`hello(name, new_)`", readme)
        self.assertIn("`on_moved(path, handler)`", readme)
        self.assertIn("`Attach`", readme)

    def run_midlc(self, *args: str) -> subprocess.CompletedProcess:
        return subprocess.run([sys.executable, str(HERE / "midlc.py"), *args],
                              capture_output=True, text=True)

    def test_check_finds_a_changed_and_a_stale_file(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            idl = Path(tmp) / "demo.midl"
            idl.write_text(DEMO, encoding="utf-8")
            out = Path(tmp) / "api"
            self.assertEqual(self.run_midlc("--rhai-api", str(out), str(idl)).returncode, 0)
            self.assertEqual(self.run_midlc("--check", "--rhai-api", str(out), str(idl)).returncode, 0)
            (out / "demo.rhai").write_text("// edited\n", encoding="utf-8")
            self.assertEqual(self.run_midlc("--check", "--rhai-api", str(out), str(idl)).returncode, 1)
            self.run_midlc("--rhai-api", str(out), str(idl))
            (out / "gone.rhai").write_text("", encoding="utf-8")
            result = self.run_midlc("--check", "--rhai-api", str(out), str(idl))
            self.assertEqual(result.returncode, 1)
            self.assertIn("gone.rhai", result.stderr)
            self.run_midlc("--rhai-api", str(out), str(idl))
            self.assertFalse((out / "gone.rhai").exists(), "regenerating removes stale modules")

    def test_the_committed_api_is_current(self) -> None:
        inputs = [str(p) for p in sorted((ROOT / "idl").glob("*.midl"))]
        result = self.run_midlc("--check", "--rhai-api", str(ROOT / "libs/rhai-lazy/api"), *inputs)
        self.assertEqual(result.returncode, 0, result.stderr)


if __name__ == "__main__":
    unittest.main()
