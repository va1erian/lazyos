#!/usr/bin/env python3
"""webd's two interfaces stay one protocol (docs/webd-plan.md section 4).

`midlc` has no imports, so the full interface (`idl/webd.midl`) declares again what it shares with the
light one (`idl/webd_light.midl`), and the two scopes declare `CreateView` and `ViewConfig` again. webd and
its client code rely on the copies being the same on the wire: a light view and a full view are served
by one table, the shared calls are decoded with one codec whatever interface they came through, and a
scope's `CreateView` is decoded with the full interface's codec. This test holds that:

* every method of the light interface exists in the full one with the same id, direction and argument
  list (same ids and types); the full one may append arguments (a field a decoder skips), never change one;
* the light interface's `CreateView` differs only in its config struct (`LightConfig`, field 1);
* each scope's `CreateView` and `ViewConfig` are the full interface's, field for field.
"""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools" / "midlc"))

from midlc_lexer import lex  # noqa: E402
from midlc_parser import Parser  # noqa: E402

LIGHT = "os.lazy.webd.light.v1"
FULL = "os.lazy.webd.view.v1"
SCOPES = ("os.lazy.webd.net.v1", "os.lazy.webd.browser.v1")


def interfaces(*files: str) -> dict:
    found = {}
    for name in files:
        text = (ROOT / "idl" / name).read_text(encoding="utf-8")
        for interface in Parser(lex(text)).parse_interfaces():
            found[interface.name] = interface
    return found


def shape(params) -> list[tuple[int, str]]:
    """A parameter list as the wire sees it: (field id, type) in id order (names are free to differ)."""
    return sorted((param.id, str(param.ty)) for param in params)


def methods(interface) -> dict:
    return {method.method_id: method for method in interface.methods}


def struct(interface, name: str):
    return next(s for s in interface.structs if s.name == name)


class SharedParts(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.all = interfaces("webd_light.midl", "webd.midl")
        cls.light, cls.full = cls.all[LIGHT], cls.all[FULL]

    def test_every_light_method_is_in_the_full_interface_unchanged(self):
        full = methods(self.full)
        for method_id, light in methods(self.light).items():
            with self.subTest(method=light.name):
                self.assertIn(method_id, full, f"{light.name} ({method_id}) is missing from {FULL}")
                other = full[method_id]
                self.assertEqual(other.name, light.name)
                self.assertEqual(other.oneway, light.oneway)
                self.assertEqual(shape(other.returns), shape(light.returns))
                ours = dict(shape(light.params))
                theirs = dict(shape(other.params))
                for field_id, ty in ours.items():
                    if light.name == "CreateView" and field_id == 1:
                        # The config struct is each interface's own (LightConfig / ViewConfig).
                        self.assertEqual((ty, theirs.get(1)), ("LightConfig", "ViewConfig"))
                        continue
                    if ty.startswith("Channel<"):
                        # Each interface's events come on its own interface.
                        self.assertTrue(theirs.get(field_id, "").startswith("Channel<"))
                        continue
                    self.assertEqual(theirs.get(field_id), ty, f"field {field_id} of {light.name}")

    def test_the_shared_enums_keep_their_values(self):
        full = {e.name: e.variants for e in self.full.enums}
        for enum in self.light.enums:
            if enum.name in full:
                with self.subTest(enum=enum.name):
                    self.assertEqual(full[enum.name][: len(enum.variants)], enum.variants)

    def test_each_scope_creates_views_exactly_as_the_full_interface_does(self):
        create = methods(self.full)[1]
        config = shape(struct(self.full, "ViewConfig").fields)
        for name in SCOPES:
            with self.subTest(scope=name):
                scope = self.all[name]
                self.assertEqual([m.method_id for m in scope.methods], [1])
                ours = scope.methods[0]
                self.assertEqual(ours.name, "CreateView")
                self.assertEqual(shape(ours.params), shape(create.params))
                self.assertEqual(shape(ours.returns), shape(create.returns))
                self.assertEqual(shape(struct(scope, "ViewConfig").fields), config)


if __name__ == "__main__":
    unittest.main()
