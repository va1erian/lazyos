#!/usr/bin/env python3
"""The guest paths baked into the libraries are ``libs/fhs``'s, and nothing
else in ``tools/webkit`` spells one out (``tools/fhs/check_literals.py`` only
reads Rust, so this is the same rule for the build scripts and the C++ proof)."""

from __future__ import annotations

import re
import sys
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
PORT = HERE.parent.parent / "webkit" / "port"
sys.path.insert(0, str(HERE))
import fhspaths  # noqa: E402
import recipes_net  # noqa: E402
import recipes_text  # noqa: E402

#: What check_literals.py calls a well-known path (plus /tmp), minus /var.
PREFIX = re.compile(r"""["'](?:/data|/docs|/home|/conf|/accounts|/apps|/logs|/system|/transient|/etc|/tmp)\b""")
#: Files that may name a path: this check and the module that reads fhs.
EXEMPT = {"test_paths.py", "fhspaths.py"}


def code_lines(path: Path):
    """(number, text) of the lines of a source file without comments."""
    comment = "//" if path.suffix in {".cpp", ".h", ".c"} else "#"
    for number, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        if comment == "//" and line.lstrip().startswith(("//", "/*", "*")):
            continue
        if comment == "#" and line.lstrip().startswith("#"):
            continue
        yield number, line


class FhsValueTests(unittest.TestCase):
    def test_fhspaths_reads_the_constants_it_names(self) -> None:
        self.assertEqual(fhspaths.get("share::FONTS"), "/system/share/fonts")
        self.assertEqual(fhspaths.get("system::SYSTEM_ETC"), "/system/etc")
        self.assertEqual(fhspaths.get("etc::LINUX_CA_BUNDLE"), "/etc/ssl/certs/ca-certificates.crt")
        self.assertEqual(fhspaths.get("etc::LINUX_SSL"), "/etc/ssl")
        self.assertEqual(fhspaths.get("mount::TMP"), "/tmp")

    def test_the_ca_bundle_lives_in_the_openssl_directory(self) -> None:
        self.assertTrue(fhspaths.CA_BUNDLE.startswith(fhspaths.OPENSSL_DIR + "/"))
        self.assertTrue(fhspaths.FONT_CACHE_DIR.startswith(fhspaths.TMP + "/"))

    def test_an_unknown_constant_is_an_error(self) -> None:
        with self.assertRaises(KeyError):
            fhspaths.get("etc::NO_SUCH_PATH")


class RecipeTests(unittest.TestCase):
    def test_fontconfig_is_configured_with_the_fhs_paths(self) -> None:
        args = recipes_text.FONTCONFIG
        self.assertIn(f"--with-default-fonts={fhspaths.get('share::FONTS')}", args)
        self.assertIn(f"--sysconfdir={fhspaths.get('system::SYSTEM_ETC')}", args)
        self.assertIn(f"--with-cache-dir={fhspaths.get('state::FONTCONFIG_CACHE')}", args)

    def test_openssl_and_curl_use_the_fhs_paths(self) -> None:
        self.assertIn(f"--openssldir={fhspaths.get('etc::LINUX_SSL')}", recipes_net.OPENSSL)
        self.assertEqual(recipes_net.CURL["CURL_CA_BUNDLE"], fhspaths.get("etc::LINUX_CA_BUNDLE"))
        self.assertEqual(recipes_net.CA_BUNDLE, fhspaths.CA_BUNDLE)

    def test_the_proof_gets_the_same_values_as_macros(self) -> None:
        self.assertEqual(fhspaths.PROOF_DEFINES["DEPSPROOF_CA_BUNDLE"], recipes_net.CURL["CURL_CA_BUNDLE"])
        self.assertEqual(fhspaths.PROOF_DEFINES["DEPSPROOF_FONT_CACHE_DIR"], recipes_text.FONT_CACHE_DIR)
        self.assertEqual(fhspaths.PROOF_DEFINES["DEPSPROOF_TMP"], fhspaths.get("mount::TMP"))

    def test_the_proof_refuses_to_compile_without_them(self) -> None:
        header = (HERE / "depsproof" / "proof.h").read_text(encoding="utf-8")
        for name in fhspaths.PROOF_DEFINES:
            self.assertIn(name, header)
        self.assertIn("#error", header)


class PortTests(unittest.TestCase):
    """The WebKit port's C++ and fonts.conf take their paths from libs/fhs, not from literals."""

    def test_the_macros_are_the_fhs_values(self) -> None:
        self.assertEqual(fhspaths.WEBKIT_DEFINES["LAZYOS_WEBPROCESS_PATH"], "/system/bin/webprocess")
        self.assertEqual(fhspaths.WEBKIT_DEFINES["LAZYOS_NETWORKPROCESS_PATH"], "/system/bin/networkprocess")
        self.assertEqual(fhspaths.WEBKIT_DEFINES["LAZYOS_CA_BUNDLE_PATH"], fhspaths.CA_BUNDLE)
        self.assertEqual(fhspaths.program("WEBKIT_SMOKE"), "/system/bin/webkit-smoke")

    def test_cmake_gets_the_macros(self) -> None:
        import port_cmake
        import toolenv
        from unittest import mock
        # The options need the zig wrappers' paths, which a host without zig
        # (CI's) cannot write: stand them in, the macros are what is tested.
        wrappers = {name: f"/wrap/{name}" for name in ("zcc", "zc++", "zar", "zranlib")}
        with mock.patch.object(toolenv, "wrappers", return_value=wrappers):
            options = port_cmake.options()
        for name, value in fhspaths.WEBKIT_DEFINES.items():
            self.assertEqual(options[name], value)

    def test_the_cpp_refuses_to_compile_without_them(self) -> None:
        sources = {
            "smoke/main.cpp": ("LAZYOS_WEBPROCESS_PATH", "LAZYOS_NETWORKPROCESS_PATH"),
            "app/engine.cpp": ("LAZYOS_WEBPROCESS_PATH", "LAZYOS_NETWORKPROCESS_PATH"),
            "tree/Source/WebKit/UIProcess/Launcher/ProcessLauncherLazyOS.cpp":
                ("LAZYOS_WEBPROCESS_PATH", "LAZYOS_NETWORKPROCESS_PATH"),
            "tree/Source/WebCore/platform/network/lazyos/CurlSSLHandleLazyOS.cpp": ("LAZYOS_CA_BUNDLE_PATH",),
        }
        for relative, names in sources.items():
            text = (PORT / relative).read_text(encoding="utf-8")
            for name in names:
                self.assertIn(f"#ifndef {name}", text, relative)
                self.assertIn("#error", text, relative)

    def test_no_port_source_names_a_guest_path(self) -> None:
        found = []
        sources = [*(PORT / "smoke").glob("*.cpp"), *(PORT / "app").glob("*.cpp"), *(PORT / "app").glob("*.h"),
                   *(PORT / "tree").rglob("*LazyOS.cpp"),
                   *(PORT / "tree").rglob("*LazyOS.cmake")]
        for path in sorted(sources):
            comment = "#" if path.suffix == ".cmake" else "//"
            for number, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
                stripped = line.lstrip()
                if stripped.startswith((comment, "/*", "*")):
                    continue
                if PREFIX.search(line):
                    found.append(f"{path.relative_to(PORT)}:{number}: {stripped}")
        self.assertEqual(found, [], "take the path from libs/fhs (fhspaths.WEBKIT_DEFINES)")

    def test_fonts_conf_is_made_from_the_template(self) -> None:
        template = (PORT / "fonts.conf.in").read_text(encoding="utf-8")
        text = fhspaths.fonts_conf(template)
        self.assertIn(f"<dir>{fhspaths.FONT_DIR}</dir>", text)
        self.assertIn(f"<cachedir>{fhspaths.FONT_CACHE_DIR}</cachedir>", text)
        self.assertEqual(re.findall(r"@\w+@", text), [])
        with self.assertRaises(KeyError):
            fhspaths.fonts_conf(template + "<dir>@OTHER@</dir>")


class NoLiteralTests(unittest.TestCase):
    def test_no_source_names_a_guest_path(self) -> None:
        found = []
        for path in sorted([*HERE.glob("*.py"), *HERE.glob("depsproof/*"), *HERE.glob("skiabuild/*")]):
            if path.name in EXEMPT or path.suffix not in {".py", ".cpp", ".h", ".c"}:
                continue
            for number, line in code_lines(path):
                if PREFIX.search(line):
                    found.append(f"{path.relative_to(HERE)}:{number}: {line.strip()}")
        self.assertEqual(found, [], "take the path from libs/fhs (fhspaths.py)")

    def test_the_check_would_catch_one(self) -> None:
        self.assertTrue(PREFIX.search('const char k[] = "/etc/ssl/certs/ca-certificates.crt";'))
        self.assertTrue(PREFIX.search("FONT_DIR = '/system/share/fonts'"))
        self.assertFalse(PREFIX.search('"fontconfig/fontconfig.h"'))


if __name__ == "__main__":
    unittest.main()
