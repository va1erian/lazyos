"""Unit tests for tools/xui/zig.py (no zig needed): python tools/xui/test_zig.py"""

from __future__ import annotations

import os
import stat
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))
import zig  # noqa: E402

TARGET = "x86_64-unknown-linux-musl"


class WrapperTests(unittest.TestCase):
    def write(self, command, windows):
        directory = Path(tempfile.mkdtemp())
        return zig.write_wrappers(command, directory, windows=windows)

    def test_posix_wrappers_exec_zig_with_the_musl_target(self):
        paths = self.write(["/opt/zig/zig"], windows=False)
        text = paths["cxx"].read_text()
        self.assertTrue(text.startswith("#!/bin/sh\n"))
        self.assertIn("exec /opt/zig/zig c++ -target x86_64-linux-musl", text)
        self.assertTrue(text.endswith('"$@"\n'))
        self.assertNotIn("\r", text)
        if os.name != "nt":  # Windows has no POSIX modes; `.cmd` needs none
            self.assertTrue(paths["cxx"].stat().st_mode & stat.S_IXUSR)

    def test_the_archiver_takes_no_target(self):
        paths = self.write(["/opt/zig/zig"], windows=False)
        self.assertIn("exec /opt/zig/zig ar", paths["ar"].read_text())
        self.assertNotIn("-target", paths["ar"].read_text())

    def test_windows_wrappers_are_cmd_files_with_crlf(self):
        paths = self.write(["C:\Program Files\zig\zig.exe"], windows=True)
        self.assertEqual(paths["cc"].suffix, ".cmd")
        text = paths["cc"].read_bytes().decode()
        self.assertEqual(
            text,
            '@"C:\Program Files\zig\zig.exe" "cc" "-target" "x86_64-linux-musl" %*\r\n',
        )

    def test_a_multi_word_command_such_as_python_m_ziglang_is_kept_whole(self):
        paths = self.write(["/usr/bin/python3", "-m", "ziglang"], windows=False)
        self.assertIn("exec /usr/bin/python3 -m ziglang cc -target", paths["cc"].read_text())

    def test_wrapper_paths_are_absolute_even_for_a_relative_directory(self):
        with tempfile.TemporaryDirectory() as work:
            cwd = os.getcwd()
            os.chdir(work)
            try:
                paths = zig.write_wrappers(["/z"], Path("rel/zig"), windows=False)
            finally:
                os.chdir(cwd)
        for path in paths.values():
            self.assertTrue(path.is_absolute(), path)

    def test_wrappers_are_never_rewritten_with_host_line_endings(self):
        paths = self.write(["/z"], windows=False)
        self.assertEqual(paths["cc"].read_bytes().count(b"\r"), 0)


class EnvTests(unittest.TestCase):
    def test_cargo_env_points_cc_ar_and_the_linker_at_the_wrappers(self):
        wrappers = {"cc": Path("/w/zcc"), "cxx": Path("/w/zcxx"), "ar": Path("/w/zar")}
        env = zig.cargo_env(TARGET, wrappers)
        self.assertEqual(env["CC_x86_64_unknown_linux_musl"], str(Path("/w/zcc")))
        self.assertEqual(env["CXX_x86_64_unknown_linux_musl"], str(Path("/w/zcxx")))
        self.assertEqual(env["AR_x86_64_unknown_linux_musl"], str(Path("/w/zar")))
        self.assertEqual(
            env["CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER"], str(Path("/w/zcxx"))
        )
        # cc-rs must not add a clang-style --target that zig cannot parse.
        self.assertEqual(env["CRATE_CC_NO_DEFAULTS"], "1")

    def test_the_link_is_static_pie_with_no_second_libc(self):
        wrappers = {"cc": Path("cc"), "cxx": Path("cxx"), "ar": Path("ar")}
        flags = zig.cargo_env(TARGET, wrappers)["CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_RUSTFLAGS"]
        self.assertIn("link-self-contained=no", flags)
        self.assertIn("-pie", flags)
        self.assertIn("-fPIC", zig.C_FLAGS)


class FindTests(unittest.TestCase):
    def test_lazyos_zig_wins_over_path(self):
        with mock.patch.dict(os.environ, {"LAZYOS_ZIG": "/custom/zig"}), mock.patch.object(
            zig, "_probe", side_effect=lambda command: "0.16.0" if command == ["/custom/zig"] else None
        ), mock.patch.object(zig.shutil, "which", return_value="/usr/bin/zig"):
            self.assertEqual(zig.find_zig(), ["/custom/zig"])

    def test_lazyos_zig_keeps_a_windows_path_with_spaces_whole(self):
        path = "C:\Program Files\zig\zig.exe"
        for value in (path, f'"{path}"', f"'{path}'", f"  {path}  "):
            seen = []
            with mock.patch.dict(os.environ, {"LAZYOS_ZIG": value}), mock.patch.object(
                zig, "_probe", side_effect=lambda command: seen.append(command) or "0.16.0"
            ):
                self.assertEqual(zig.find_zig(), [path], value)
            self.assertEqual(seen, [[path]])

    def test_falls_back_to_the_pip_wheel(self):
        env = {k: v for k, v in os.environ.items() if k != "LAZYOS_ZIG"}
        wheel = [sys.executable, "-m", "ziglang"]
        with mock.patch.dict(os.environ, env, clear=True), mock.patch.object(
            zig.shutil, "which", return_value=None
        ), mock.patch.object(zig, "_probe", side_effect=lambda c: "0.16.0" if c == wheel else None):
            self.assertEqual(zig.find_zig(), wheel)

    def test_none_when_zig_is_missing(self):
        env = {k: v for k, v in os.environ.items() if k != "LAZYOS_ZIG"}
        with mock.patch.dict(os.environ, env, clear=True), mock.patch.object(
            zig.shutil, "which", return_value=None
        ), mock.patch.object(zig, "_probe", return_value=None):
            self.assertIsNone(zig.find_zig())


if __name__ == "__main__":
    unittest.main()
