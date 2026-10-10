#!/usr/bin/env python3
"""Tests for field ids, the standard error field and the conformance corpus.

Run: python tools/midlc/test_midlc_fields.py
"""

from __future__ import annotations

import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import midlc  # noqa: E402
from midlc_conformance import expected_files  # noqa: E402
from midlc_errors import ERRORS_SUPPORT  # noqa: E402
from midlc_pin import pin_text  # noqa: E402
from midlc_wire import encode_error, encode_message  # noqa: E402

HERE = Path(__file__).resolve().parent
CORPUS = HERE.parents[1] / "idl" / "conformance"


def parse(text: str) -> tuple[midlc.Interface, list[str]]:
    parser = midlc.Parser(midlc.lex(text))
    return parser.parse_interface(), parser.warnings


def wrap(body: str) -> str:
    return f"interface os.lazy.t.v1 {{\n    {body}\n}}\n"


class FieldIdTests(unittest.TestCase):
    def test_explicit_ids_win_and_implicit_ones_are_positions(self) -> None:
        interface, _ = parse(wrap("struct P { a: U32 = 9, b: U32, c: U32 = 4 }"))
        self.assertEqual([f.id for f in interface.structs[0].fields], [9, 2, 4])

    def test_implicit_ids_warn_and_explicit_ones_do_not(self) -> None:
        _, warnings = parse(wrap("method M(a: U32, b: U32 = 2) -> (r: U32 = 1);"))
        self.assertEqual(len(warnings), 1)
        self.assertIn("'a' has the implicit id 1", warnings[0])

    def test_rejections(self) -> None:
        for body, needle in [
            ("struct P { a: U32 = 1, b: U32 = 1 }", "both have id 1"),
            ("struct P { a: U32 = 0 }", "outside 1..65535"),
            ("struct P { a: U32 = 70000 }", "outside 1..65535"),
            ("method M() -> (a: U32 = 15);", "reserved for the standard error"),
            ("method M() -> () transfers (b: Buffer);", "`transfers (...)` clause is gone"),
            ("struct P { a: U32 = x }", "expected a field id"),
            ("struct P { m: Map<String, U32> }", "no Map type"),
        ]:
            with self.subTest(body=body), self.assertRaises(midlc.MidlError) as caught:
                parse(wrap(body))
            self.assertIn(needle, str(caught.exception))

    def test_an_argument_may_use_the_error_field_id(self) -> None:
        interface, _ = parse(wrap("method M(a: U32 = 15) -> ();"))
        self.assertEqual(interface.methods[0].params[0].id, 15)

    def test_codegen_writes_and_dispatches_on_the_declared_ids(self) -> None:
        interface, _ = parse(wrap("struct P { a: U32 = 7, b: String = 3 }"))
        rust = midlc.emit_rust(interface)
        self.assertIn("target.u32(7, value.a)?;", rust)
        self.assertIn("target.string(3, &value.b)?;", rust)
        self.assertIn("7 => {", rust)
        self.assertIn("3 => {", rust)

    def test_enum_typed_fields_travel_as_u32(self) -> None:
        interface, _ = parse(wrap("enum L { A, B }\n    struct P { l: L = 1, ls: Array<L> = 2 }"))
        rust = midlc.emit_rust(interface)
        self.assertIn("pub l: u32,", rust)
        self.assertIn("pub ls: alloc::vec::Vec<u32>,", rust)
        self.assertIn("target.u32(1, value.l)?;", rust)
        self.assertNotIn("decode_l(", rust)


class PinTests(unittest.TestCase):
    def test_pin_writes_positions_and_keeps_the_rest(self) -> None:
        text = wrap("/// doc\n    method M(a: U32, b: Array<String> = 5) -> (r: Option<U64>); // c\n    struct S { x: I32 }")
        pinned, count = pin_text(text)
        self.assertEqual(count, 3)
        self.assertIn("method M(a: U32 = 1, b: Array<String> = 5) -> (r: Option<U64> = 1); // c", pinned)
        self.assertIn("struct S { x: I32 = 1 }", pinned)
        self.assertEqual(pin_text(pinned), (pinned, 0))
        before, _ = parse(text)
        after, warnings = parse(pinned)
        self.assertEqual(midlc.emit_rust(before), midlc.emit_rust(after))
        self.assertEqual(warnings, [])

    def test_every_checked_in_interface_is_pinned(self) -> None:
        warnings: list[str] = []
        midlc.parse_all(sorted((HERE.parents[1] / "idl").glob("*.midl")), warnings)
        self.assertEqual(warnings, [])


class ErrorFieldTests(unittest.TestCase):
    def test_the_generated_crate_carries_the_errors_module(self) -> None:
        self.assertIn("pub const ERROR_FIELD: u16 = 15;", ERRORS_SUPPORT)
        self.assertNotIn("__", ERRORS_SUPPORT)

    def test_reference_error_encoding(self) -> None:
        self.assertEqual(encode_error(15, {"code": 2, "message": "no"}).hex(), "0d0f00000600000002000000" + "6e6f")
        detailed = encode_error(15, {"code": 1, "message": "x", "domain": "d"})
        self.assertEqual(detailed[8:], bytes([1, 0, 0, 0]) + b"x\0" + bytes.fromhex("0701000001000000") + b"d")


class WireTests(unittest.TestCase):
    def test_fields_encode_under_their_ids(self) -> None:
        interface, _ = parse(wrap("struct P { a: U32 = 300, o: Option<Bool> = 2 }"))
        body, objects = encode_message(interface, "P", {"a": 1, "o": None})
        self.assertEqual(body.hex(), "042c0100" "04000000" "01000000" "0c020000" "00000000")
        self.assertEqual(objects, [])

    def test_a_record_value_must_name_every_field(self) -> None:
        interface, _ = parse(wrap("struct P { a: U32 = 1 }"))
        with self.assertRaises(midlc.MidlError):
            encode_message(interface, "P", {})


class CorpusTests(unittest.TestCase):
    def test_the_checked_in_corpus_is_current(self) -> None:
        result = subprocess.run(
            [sys.executable, str(HERE / "conformance.py"), "--check"], capture_output=True, text=True
        )
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_check_fails_when_an_expected_file_drifts(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            corpus = Path(tmp) / "corpus"
            shutil.copytree(CORPUS, corpus)
            rust = Path(tmp) / "conformance.rs"
            command = [sys.executable, str(HERE / "conformance.py"), "--corpus", str(corpus), "--rust", str(rust)]
            self.assertEqual(subprocess.run(command + ["--write"], capture_output=True).returncode, 0)
            self.assertEqual(subprocess.run(command + ["--check"], capture_output=True).returncode, 0)
            target = corpus / "valid" / "errors.expected.json"
            target.write_text(target.read_text(encoding="utf-8").replace("cafe", "beef"), encoding="utf-8")
            self.assertEqual(subprocess.run(command + ["--check"], capture_output=True).returncode, 1)

    def test_every_invalid_case_is_rejected_and_every_valid_one_has_vectors(self) -> None:
        files = expected_files(CORPUS)
        invalid = [p for p in files if p.parent.name == "invalid"]
        self.assertGreaterEqual(len(invalid), 10)
        self.assertTrue(all('"error"' in files[p] for p in invalid))
        valid = [p for p in files if p.parent.name == "valid"]
        self.assertTrue(all('"bytes"' in files[p] for p in valid))


if __name__ == "__main__":
    unittest.main()
