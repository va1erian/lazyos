"""Tests for idl_compat.compare: appending is allowed, everything else is reported."""

from __future__ import annotations

import copy
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from idl_compat import compare  # noqa: E402

BASE = {
    "os.lazy.demo.v1": {
        "methods": {
            "Get": {"id": 7, "oneway": False, "params": {1: ("key", "String")}, "returns": {1: ("value", "String")}},
        },
        "structs": {"Item": {1: ("name", "String")}},
        "enums": {"Level": ["Info", "Warn"]},
        "topics": {},
    }
}


def changed(edit) -> list[str]:
    new = copy.deepcopy(BASE)
    edit(new["os.lazy.demo.v1"])
    return compare(BASE, new)


class CompareTests(unittest.TestCase):
    def test_unchanged_is_compatible(self) -> None:
        self.assertEqual(compare(BASE, copy.deepcopy(BASE)), [])

    def test_appending_is_compatible(self) -> None:
        def append(itf):
            itf["methods"]["Put"] = {"id": 8, "oneway": False, "params": {}, "returns": {}}
            itf["methods"]["Get"]["params"][2] = ("flags", "U32")
            itf["methods"]["Get"]["returns"][2] = ("age", "U64")
            itf["structs"]["Item"][2] = ("size", "U64")
            itf["enums"]["Level"].append("Error")

        self.assertEqual(changed(append), [])

    def test_breaking_changes_are_reported(self) -> None:
        cases = {
            "method removed": lambda i: i["methods"].pop("Get"),
            "method id": lambda i: i["methods"]["Get"].update(id=9),
            "oneway": lambda i: i["methods"]["Get"].update(oneway=True),
            "arg removed": lambda i: i["methods"]["Get"]["params"].clear(),
            "arg retyped": lambda i: i["methods"]["Get"]["params"].update({1: ("key", "U32")}),
            "reply retyped": lambda i: i["methods"]["Get"]["returns"].update({1: ("value", "Bytes")}),
            "struct field removed": lambda i: i["structs"]["Item"].clear(),
            "struct removed": lambda i: i["structs"].pop("Item"),
            "enum reordered": lambda i: i["enums"].update(Level=["Warn", "Info"]),
            "enum variant removed": lambda i: i["enums"].update(Level=["Info"]),
        }
        for name, edit in cases.items():
            with self.subTest(name):
                self.assertTrue(changed(edit), name)

    def test_removed_interface_is_reported(self) -> None:
        self.assertTrue(compare(BASE, {}))

    def test_rename_alone_is_not_breaking(self) -> None:
        self.assertEqual(changed(lambda i: i["structs"]["Item"].update({1: ("title", "String")})), [])


if __name__ == "__main__":
    unittest.main()
