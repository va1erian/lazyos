#!/usr/bin/env python3
"""``licenses.py`` accepts the bundle as pinned and fails when it should."""

from __future__ import annotations

import dataclasses
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import fhspaths  # noqa: E402
import licenses  # noqa: E402
import wkpins  # noqa: E402

NETTLS = licenses.nettls_licenses()


def with_license(name: str, expression: str, notes: str | None = None) -> wkpins.Dep:
    dep = wkpins.BY_NAME[name]
    return dataclasses.replace(dep, license=expression, notes=dep.notes if notes is None else notes)


class VerdictTests(unittest.TestCase):
    def problem(self, dep: wkpins.Dep) -> str:
        return licenses.verdict(dep, NETTLS)[2]

    def test_every_pinned_library_is_acceptable(self) -> None:
        for dep in licenses.libraries():
            self.assertEqual(self.problem(dep), "", dep.name)

    def test_openssl_is_apache_and_gplv2_incompatible(self) -> None:
        dep = wkpins.BY_NAME["openssl"]
        self.assertEqual(dep.license, "Apache-2.0")
        self.assertEqual(licenses.verdict(dep, NETTLS)[:2], (True, False))
        self.assertIn("openssl", wkpins.NEVER_IN_GPL2)
        self.assertIn("nettls", dep.notes)

    def test_webkit_itself_is_judged_and_registered(self) -> None:
        dep = wkpins.WEBKIT
        self.assertIn(dep, licenses.libraries())
        self.assertEqual(dep.license, "LGPL-2.1-or-later AND BSD-2-Clause")
        self.assertEqual(self.problem(dep), "")
        self.assertEqual(licenses.verdict(dep, NETTLS)[:2], (True, True))
        rows = [row for row in licenses.rows() if row.startswith("| WebKit 2.54.1 |")]
        self.assertEqual(len(rows), 1)
        self.assertIn("webprocess", rows[0])

    def test_port_only_libraries_say_they_are_linked_into_the_port(self) -> None:
        by_name = {row.split("|")[1].strip().split()[0]: row for row in licenses.rows()}
        for name in ("libwpe", "glstub", "WebKit"):
            self.assertNotIn("depsproof", by_name[name], name)
            self.assertIn(fhspaths.program("WEBPROCESS"), by_name[name], name)
        self.assertIn("depsproof", by_name["zlib"])

    def test_the_port_only_rows_name_webds_program(self) -> None:
        rows = {row.split("|")[1].strip().split()[0]: row for row in licenses.rows()}
        for name in ("libwpe", "glstub", "WebKit"):
            self.assertIn("webd-app", rows[name], name)
            self.assertIn("LAZYOS_WEBD=1", rows[name], name)
            self.assertNotIn("lazyweb-app", rows[name], name)

    def test_lazyweb_crates_are_offered_under_both_licences_and_the_checked_in_tree_passes(self) -> None:
        self.assertEqual(licenses.check_lazyweb_licence(), [])
        for name in licenses.LAZYWEB_CRATES:
            self.assertTrue((licenses.ROOT / name).is_file(), name)

    def test_a_gpl2_only_crate_is_refused(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            crate = root / "xui-app" / "lazyweb"
            crate.mkdir(parents=True)
            (crate / "Cargo.toml").write_text('[package]\nname = "x"\nlicense = "GPL-2.0-only"\n', encoding="utf-8")
            original = licenses.ROOT
            licenses.ROOT = root
            try:
                problems = licenses.check_lazyweb_licence()
            finally:
                licenses.ROOT = original
        self.assertTrue(any("GPL-3.0-or-later" in p for p in problems), problems)

    def test_an_and_or_a_third_licence_is_not_the_promised_choice(self) -> None:
        for expression in ("GPL-2.0-only AND GPL-3.0-or-later", "GPL-2.0-only OR GPL-3.0-or-later OR MIT",
                           "GPL-2.0-only OR GPL-3.0-only"):
            with tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                crate = root / "xui-app" / "lazyweb"
                crate.mkdir(parents=True)
                (crate / "Cargo.toml").write_text(f'[package]\nname = "x"\nlicense = "{expression}"\n', encoding="utf-8")
                original = licenses.ROOT
                licenses.ROOT = root
                try:
                    problems = licenses.check_lazyweb_licence()
                finally:
                    licenses.ROOT = original
            self.assertTrue(problems, expression)

    def test_a_table_form_openssl_dependency_is_caught(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            crate = root / "xui-app" / "lazyweb"
            crate.mkdir(parents=True)
            (crate / "Cargo.toml").write_text(
                '[package]\nname = "x"\nlicense = "GPL-2.0-only OR GPL-3.0-or-later"\n[dependencies.openssl]\nversion = "1"\n',
                encoding="utf-8")
            original = licenses.ROOT
            licenses.ROOT = root
            try:
                problems = licenses.check_lazyweb_licence()
            finally:
                licenses.ROOT = original
        self.assertTrue(any("OpenSSL" in p for p in problems), problems)

    def test_libpsl_records_the_list_licence_too(self) -> None:
        self.assertEqual(wkpins.BY_NAME["libpsl"].license, "MIT AND MPL-2.0")

    def test_apache_is_refused_for_any_other_library(self) -> None:
        self.assertIn("not on the LGPL-2.1 allowlist", self.problem(with_license("zlib", "Apache-2.0")))

    def test_an_unknown_licence_is_refused(self) -> None:
        self.assertIn("allowlist", self.problem(with_license("curl", "Proprietary")))
        self.assertIn("allowlist", self.problem(with_license("curl", "MIT AND GPL-3.0-only")))
        self.assertIn("curl", self.problem(with_license("curl", "")))

    def test_an_or_needs_one_acceptable_side(self) -> None:
        self.assertEqual(self.problem(with_license("freetype", "Proprietary OR MIT")), "")

    def test_a_malformed_expression_is_a_problem_not_a_crash(self) -> None:
        self.assertIn("freetype", self.problem(with_license("freetype", "MIT AND (")))

    def test_openssl_must_say_why_it_is_exempt(self) -> None:
        self.assertIn("nettls", self.problem(with_license("openssl", "Apache-2.0", notes="")))

    def test_a_gplv2_compatible_library_cannot_hide_in_the_exemption(self) -> None:
        original = wkpins.NEVER_IN_GPL2
        wkpins.NEVER_IN_GPL2 = ("openssl", "zlib")
        try:
            self.assertIn("NEVER_IN_GPL2", self.problem(wkpins.BY_NAME["zlib"]))
        finally:
            wkpins.NEVER_IN_GPL2 = original

    def test_a_gplv2_incompatible_library_must_be_listed(self) -> None:
        original = wkpins.NEVER_IN_GPL2
        wkpins.NEVER_IN_GPL2 = ()
        try:
            self.assertIn("allowlist", self.problem(wkpins.BY_NAME["openssl"]))
        finally:
            wkpins.NEVER_IN_GPL2 = original


class RegisterTests(unittest.TestCase):
    def test_the_checked_in_register_has_every_row(self) -> None:
        text = licenses.THIRD_PARTY.read_text(encoding="utf-8")
        self.assertEqual(licenses.check_register(text), [])
        self.assertEqual(licenses.check_notices(text, licenses.README.read_text(encoding="utf-8")), [])

    def test_the_generated_rows_satisfy_the_check(self) -> None:
        self.assertEqual(licenses.check_register("\n".join(licenses.rows())), [])

    def test_a_missing_row_is_named(self) -> None:
        rows = [row for row in licenses.rows() if not row.startswith("| curl ")]
        problems = licenses.check_register("\n".join(rows))
        self.assertEqual(len(problems), 1)
        self.assertIn("curl 8.22.0", problems[0])

    def test_a_row_with_another_digest_is_refused(self) -> None:
        rows = [row.replace(wkpins.BY_NAME["zlib"].source.sha256, "0" * 64) for row in licenses.rows()]
        self.assertIn("zlib", " ".join(licenses.check_register("\n".join(rows))))

    def test_a_row_with_another_licence_is_refused(self) -> None:
        rows = [row.replace("| MIT AND MPL-2.0;", "| MIT;") for row in licenses.rows()]
        self.assertIn("libpsl", " ".join(licenses.check_register("\n".join(rows))))

    def test_openssl_must_be_called_out_in_both_documents(self) -> None:
        third_party = "| openssl 3.5.9 | url | Apache-2.0 | linked |"
        readme = "OpenSSL is Apache-2.0."
        self.assertEqual(len(licenses.check_notices(third_party, readme)), 2)
        ok = "OpenSSL (Apache-2.0) must never be linked into nettls."
        self.assertEqual(licenses.check_notices(ok, ok), [])


class ManifestTests(unittest.TestCase):
    def test_a_build_with_the_pinned_licences_passes(self) -> None:
        records = [{"name": dep.name, "license": dep.license} for dep in licenses.libraries()]
        self.assertEqual(licenses.check_manifest({"libraries": records}), [])

    def test_a_stale_licence_or_unknown_library_is_named(self) -> None:
        problems = licenses.check_manifest({"libraries": [
            {"name": "libpsl", "license": "MIT"}, {"name": "mystery", "license": "MIT"}]})
        self.assertEqual(len(problems), 2)
        self.assertIn("rebuild", problems[0])
        self.assertIn("mystery", problems[1])


if __name__ == "__main__":
    unittest.main()
