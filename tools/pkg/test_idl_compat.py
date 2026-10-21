"""Tests for idl_compat: appending is allowed, everything else is reported."""

from __future__ import annotations

import copy
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from idl_compat import compare, schema_of_text  # noqa: E402

V1 = """
interface os.lazy.demo.v1 {
    struct Item { name: String = 1 }
    enum Level { Info, Warn }
    method Get(key: String = 1) -> (value: String = 1) = 7;
    method Watch(events: Channel<os.lazy.demo.v1> = 1) -> ();
    topic "demo/{key}/changed" : Item qos=latest;
}
"""


def parse(text: str) -> dict:
    return schema_of_text(text)


def changed(edit) -> list[str]:
    """Problems after applying `edit` to the parsed V1 schema."""
    new = copy.deepcopy(parse(V1))
    edit(new["os.lazy.demo.v1"])
    return compare(parse(V1), new)


class ParseTests(unittest.TestCase):
    def test_real_midl_text_gives_ids_types_and_objects(self) -> None:
        itf = parse(V1)["os.lazy.demo.v1"]
        self.assertEqual(itf["methods"]["Get"]["id"], 7)
        self.assertEqual(itf["methods"]["Get"]["params"], {1: ("key", "String", False)})
        self.assertTrue(itf["methods"]["Watch"]["params"][1][2], "a Channel parameter carries an object")
        self.assertEqual(len(itf["methods"]["Watch"]["objects"]), 1)
        self.assertEqual(itf["enums"]["Level"], ["Info", "Warn"])
        self.assertIn("demo/+/changed", itf["topics"])

    def test_the_checked_in_idl_compares_clean_against_itself(self) -> None:
        from idl_compat import schema

        now = schema(None)
        self.assertGreater(len(now), 20)
        self.assertEqual(compare(now, now), [])


class AppendTests(unittest.TestCase):
    def test_appending_is_compatible(self) -> None:
        new_text = V1.replace(
            "method Get(key: String = 1) -> (value: String = 1) = 7;",
            "method Get(key: String = 1, flags: U32 = 2) -> (value: String = 1, age: U64 = 2) = 7;\n"
            "    method Put(key: String = 1) -> ();",
        ).replace("struct Item { name: String = 1 }", "struct Item { name: String = 1, size: U64 = 2 }")
        new_text = new_text.replace("enum Level { Info, Warn }", "enum Level { Info, Warn, Error }")
        self.assertEqual(compare(parse(V1), parse(new_text)), [])

    def test_a_pure_rename_is_compatible(self) -> None:
        new_text = V1.replace("struct Item { name: String = 1 }", "struct Item { title: String = 1 }")
        self.assertEqual(compare(parse(V1), parse(new_text)), [])


class BreakTests(unittest.TestCase):
    def test_breaking_changes_are_reported(self) -> None:
        cases = {
            "method removed": lambda i: i["methods"].pop("Get"),
            "method id": lambda i: i["methods"]["Get"].update(id=9),
            "oneway": lambda i: i["methods"]["Get"].update(oneway=True),
            "arg removed": lambda i: i["methods"]["Get"]["params"].clear(),
            "arg retyped": lambda i: i["methods"]["Get"]["params"].update({1: ("key", "U32", False)}),
            "reply retyped": lambda i: i["methods"]["Get"]["returns"].update({1: ("value", "Bytes", False)}),
            "struct field removed": lambda i: i["structs"]["Item"].clear(),
            "struct removed": lambda i: i["structs"].pop("Item"),
            "enum reordered": lambda i: i["enums"].update(Level=["Warn", "Info"]),
            "enum variant removed": lambda i: i["enums"].update(Level=["Info"]),
            "topic payload": lambda i: i["topics"].update({"demo/+/changed": ("Other", "latest", False, ("+",))}),
            "ring added to the object list": lambda i: i["methods"]["Get"]["objects"].append(("buffer", None, "b")),
        }
        for name, edit in cases.items():
            with self.subTest(name):
                self.assertTrue(changed(edit), name)

    def test_renaming_a_topic_wildcard_is_compatible(self) -> None:
        new_text = V1.replace("demo/{key}/changed", "demo/{id}/changed")
        self.assertEqual(compare(parse(V1), parse(new_text)), [])

    def test_removed_interface_is_reported(self) -> None:
        self.assertTrue(compare(parse(V1), {}))

    def test_a_swap_of_two_same_typed_fields_is_reported(self) -> None:
        old = "interface os.lazy.s.v1 { struct S { x: U32 = 1, y: U32 = 2 } }"
        new = "interface os.lazy.s.v1 { struct S { y: U32 = 1, x: U32 = 2 } }"
        problems = compare(parse(old), parse(new))
        self.assertTrue(problems)
        self.assertTrue(all("S" in p for p in problems))

    def test_a_field_moved_to_another_id_is_reported(self) -> None:
        old = "interface os.lazy.s.v1 { struct S { x: U32 = 1 } }"
        new = "interface os.lazy.s.v1 { struct S { x: U32 = 3 } }"
        self.assertIn("moved from id 1 to id 3", " ".join(compare(parse(old), parse(new))))

    def test_appending_an_object_argument_breaks_the_object_list(self) -> None:
        new_text = V1.replace(
            "method Watch(events: Channel<os.lazy.demo.v1> = 1) -> ();",
            "method Watch(events: Channel<os.lazy.demo.v1> = 1, more: Channel<os.lazy.demo.v1> = 2) -> ();",
        )
        problems = compare(parse(V1), parse(new_text))
        self.assertTrue(any("object list" in p for p in problems))
        self.assertTrue(any("carries a kernel object" in p for p in problems))

    def test_a_ring_change_is_reported(self) -> None:
        # A ring needs a carrying method to parse, so edit the parsed schema.
        old = parse(V1)
        old["os.lazy.demo.v1"]["rings"]["Rx"] = ("frames", "server", "Notify", None)
        new = copy.deepcopy(old)
        new["os.lazy.demo.v1"]["rings"]["Rx"] = ("frames", "client", "Notify", None)
        self.assertTrue(any("ring Rx" in p for p in compare(old, new)))
        self.assertEqual(compare(old, copy.deepcopy(old)), [])


if __name__ == "__main__":
    unittest.main()
