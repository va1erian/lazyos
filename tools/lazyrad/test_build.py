"""Unit tests for tools/lazyrad/build.py (no cargo needed).

Run: ``python tools/lazyrad/test_build.py``
"""

from __future__ import annotations

import contextlib
import io
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))
import build  # noqa: E402

TARGET = "x86_64-unknown-linux-musl"
PREFIX = "CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL"


class ArgumentTests(unittest.TestCase):
    def test_default_builds_all_in_release(self):
        args = build.parse_args([])
        self.assertEqual(args.bin, "all")
        self.assertFalse(args.debug)
        self.assertEqual(build.select_bins(args.bin), ["lrplay", "lazyrad"])

    def test_bin_selects_a_single_binary(self):
        for name in ("lrplay", "lazyrad"):
            args = build.parse_args(["--bin", name])
            self.assertEqual(build.select_bins(args.bin), [name])

    def test_bin_all_is_explicitly_accepted(self):
        args = build.parse_args(["--bin", "all"])
        self.assertEqual(build.select_bins(args.bin), ["lrplay", "lazyrad"])

    def test_debug_selects_the_debug_profile(self):
        self.assertEqual(build.profile(build.parse_args(["--debug"]).debug), "debug")
        self.assertEqual(build.profile(build.parse_args([]).debug), "release")

    def test_an_unknown_bin_is_rejected(self):
        with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit) as raised:
            build.parse_args(["--bin", "nope"])
        self.assertEqual(raised.exception.code, 2)


class EnvTests(unittest.TestCase):
    def test_posix_leaves_the_environment_alone(self):
        with mock.patch.dict(os.environ, {"PATH": "/usr/bin"}, clear=True):
            self.assertEqual(build.build_env(os_name="posix"), {"PATH": "/usr/bin"})

    def test_windows_points_at_the_bundled_lld_for_the_target_only(self):
        with tempfile.TemporaryDirectory() as sysroot:
            host = "x86_64-pc-windows-msvc"
            linker = build.rust_lld_path(sysroot, host)
            linker.parent.mkdir(parents=True)
            linker.write_bytes(b"lld")
            with mock.patch.dict(os.environ, {"PATH": "C:\\bin"}, clear=True):
                env = build.build_env(os_name="nt", sysroot=sysroot, host=host)
        self.assertEqual(env[f"{PREFIX}_LINKER"], str(linker))
        self.assertEqual(env[f"{PREFIX}_RUSTFLAGS"], "-C linker-flavor=ld.lld")
        # A bare RUSTFLAGS would leak into host build scripts and proc macros.
        self.assertNotIn("RUSTFLAGS", env)

    def test_windows_without_lld_sets_nothing(self):
        with tempfile.TemporaryDirectory() as sysroot:
            with mock.patch.dict(os.environ, {"PATH": "C:\\bin"}, clear=True):
                env = build.build_env(os_name="nt", sysroot=sysroot, host="x86_64-pc-windows-msvc")
        self.assertEqual(env, {"PATH": "C:\\bin"})

    def test_existing_target_variables_win(self):
        with tempfile.TemporaryDirectory() as sysroot:
            host = "x86_64-pc-windows-msvc"
            build.rust_lld_path(sysroot, host).parent.mkdir(parents=True)
            build.rust_lld_path(sysroot, host).write_bytes(b"lld")
            given = {f"{PREFIX}_LINKER": "C:\\custom\\link.exe", f"{PREFIX}_RUSTFLAGS": "-C foo"}
            with mock.patch.dict(os.environ, given, clear=True):
                env = build.build_env(os_name="nt", sysroot=sysroot, host=host)
        self.assertEqual(env[f"{PREFIX}_LINKER"], "C:\\custom\\link.exe")
        self.assertEqual(env[f"{PREFIX}_RUSTFLAGS"], "-C foo")

    def test_a_sysroot_with_spaces_is_kept_whole(self):
        with tempfile.TemporaryDirectory(prefix="rust up ") as sysroot:
            host = "x86_64-pc-windows-msvc"
            linker = build.rust_lld_path(sysroot, host)
            linker.parent.mkdir(parents=True)
            linker.write_bytes(b"lld")
            with mock.patch.dict(os.environ, {}, clear=True):
                env = build.build_env(os_name="nt", sysroot=sysroot, host=host)
        self.assertEqual(env[f"{PREFIX}_LINKER"], str(linker))
        self.assertIn(" ", env[f"{PREFIX}_LINKER"])

    def test_host_triple_reads_the_rustc_banner(self):
        banner = "rustc 1.80.0\nbinary: rustc\ncommit-hash: abc\nhost: x86_64-pc-windows-msvc\n"
        self.assertEqual(build.host_triple(banner), "x86_64-pc-windows-msvc")
        self.assertEqual(build.host_triple("rustc 1.80.0\n"), "")

    def test_target_env_prefix_uses_underscores(self):
        self.assertEqual(build.target_env_prefix("a-b-c"), "CARGO_TARGET_A_B_C")


class PathTests(unittest.TestCase):
    def test_source_path_is_inside_the_lazyrad_workspace(self):
        self.assertEqual(
            build.source_path("release", "lrplay"),
            build.APP / "target" / TARGET / "release" / "lrplay",
        )
        self.assertEqual(
            build.source_path("debug", "lazyrad"),
            build.APP / "target" / TARGET / "debug" / "lazyrad",
        )

    def test_output_names_are_the_elf_names_under_target_lazyrad(self):
        self.assertEqual(build.dest_path(build.BINS["lrplay"]), build.OUT_DIR / "lrplay.elf")
        self.assertEqual(build.dest_path(build.BINS["lazyrad"]), build.OUT_DIR / "lazyrad.elf")
        self.assertEqual(build.OUT_DIR, build.ROOT / "target" / "lazyrad")


class MainTests(unittest.TestCase):
    def make_app(self, root: Path) -> tuple[Path, Path]:
        app = root / "lazyrad-os"
        manifest = app / "Cargo.toml"
        manifest.parent.mkdir(parents=True)
        manifest.write_text("[workspace]\n")
        return app, manifest

    def run_main(self, app: Path, manifest: Path, out: Path, argv: list[str]):
        calls: list[list[str]] = []

        def fake_run(cmd, env=None):
            calls.append(cmd)
            return subprocess.CompletedProcess(cmd, 0, "", "")

        with mock.patch.object(build, "APP", app), mock.patch.object(
            build, "MANIFEST", manifest
        ), mock.patch.object(build, "OUT_DIR", out), mock.patch.object(
            build, "ensure_target", return_value=True
        ), mock.patch.object(build, "build_env", return_value={}), mock.patch.object(
            build, "run", side_effect=fake_run
        ), contextlib.redirect_stdout(io.StringIO()) as stdout:
            code = build.main(argv)
        return code, calls, stdout.getvalue()

    def test_missing_app_fails_before_touching_the_toolchain(self):
        missing = Path("does/not/exist/Cargo.toml")
        with mock.patch.object(build, "MANIFEST", missing), mock.patch.object(
            build, "ensure_target"
        ) as ensure, contextlib.redirect_stderr(io.StringIO()) as stderr:
            self.assertEqual(build.main([]), 1)
        ensure.assert_not_called()
        self.assertIn("lazyrad-os", stderr.getvalue())

    def test_missing_musl_target_skips_with_an_empty_map(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            app, manifest = self.make_app(root)
            out = root / "target" / "lazyrad"
            with mock.patch.object(build, "APP", app), mock.patch.object(
                build, "MANIFEST", manifest
            ), mock.patch.object(build, "OUT_DIR", out), mock.patch.object(
                build, "ensure_target", return_value=False
            ), mock.patch.object(build, "run") as run, contextlib.redirect_stdout(
                io.StringIO()
            ) as stdout:
                self.assertEqual(build.main([]), 0)
            run.assert_not_called()
            self.assertEqual(json.loads(stdout.getvalue()), {})

    def test_builds_and_copies_only_the_requested_bin(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            app, manifest = self.make_app(root)
            out = root / "target" / "lazyrad"
            source = app / "target" / TARGET / "release" / "lrplay"
            source.parent.mkdir(parents=True)
            source.write_bytes(b"\x7fELF-lrplay")
            code, calls, stdout = self.run_main(app, manifest, out, ["--bin", "lrplay"])
            copied = (out / "lrplay.elf").read_bytes()
            other = (out / "lazyrad.elf").exists()
            mapping = json.loads(stdout)
        self.assertEqual(code, 0)
        self.assertEqual(
            calls,
            [
                [
                    "cargo",
                    "build",
                    "--manifest-path",
                    str(manifest),
                    "--target",
                    TARGET,
                    "--bin",
                    "lrplay",
                    "--release",
                ]
            ],
        )
        self.assertEqual(copied, b"\x7fELF-lrplay")
        self.assertFalse(other)
        self.assertEqual(mapping, {"lrplay": str(out / "lrplay.elf")})

    def test_a_missing_artifact_fails_instead_of_a_silent_empty_image(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            app, manifest = self.make_app(root)
            out = root / "target" / "lazyrad"
            with contextlib.redirect_stderr(io.StringIO()) as stderr:
                code, _, _ = self.run_main(app, manifest, out, ["--bin", "lrplay"])
        self.assertEqual(code, 1)
        self.assertIn("was not produced", stderr.getvalue())
        self.assertFalse((out / "lrplay.elf").exists())

    def test_a_partial_build_copies_nothing(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            app, manifest = self.make_app(root)
            out = root / "target" / "lazyrad"
            # `lrplay` built, `lazyrad` missing: nothing may be copied.
            source = app / "target" / TARGET / "release" / "lrplay"
            source.parent.mkdir(parents=True)
            source.write_bytes(b"\x7fELF-lrplay")
            with contextlib.redirect_stderr(io.StringIO()):
                code, _, _ = self.run_main(app, manifest, out, [])
        self.assertEqual(code, 1)
        self.assertFalse((out / "lrplay.elf").exists())
        self.assertFalse((out / "lazyrad.elf").exists())

    def test_debug_omits_release_and_uses_the_debug_directory(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            app, manifest = self.make_app(root)
            out = root / "target" / "lazyrad"
            source = app / "target" / TARGET / "debug" / "lazyrad"
            source.parent.mkdir(parents=True)
            source.write_bytes(b"\x7fELF-lazyrad")
            code, calls, _ = self.run_main(app, manifest, out, ["--bin", "lazyrad", "--debug"])
            copied = (out / "lazyrad.elf").read_bytes()
        self.assertEqual(code, 0)
        self.assertNotIn("--release", calls[0])
        self.assertEqual(copied, b"\x7fELF-lazyrad")

    def test_a_missing_linker_skips_instead_of_failing(self):
        def fake_run(cmd, env=None):
            return subprocess.CompletedProcess(cmd, 1, "", "error: linker `cc` not found")

        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            app, manifest = self.make_app(root)
            out = root / "target" / "lazyrad"
            with mock.patch.object(build, "APP", app), mock.patch.object(
                build, "MANIFEST", manifest
            ), mock.patch.object(build, "OUT_DIR", out), mock.patch.object(
                build, "ensure_target", return_value=True
            ), mock.patch.object(build, "build_env", return_value={}), mock.patch.object(
                build, "run", side_effect=fake_run
            ), contextlib.redirect_stderr(io.StringIO()), contextlib.redirect_stdout(
                io.StringIO()
            ) as stdout:
                self.assertEqual(build.main([]), 0)
            self.assertEqual(json.loads(stdout.getvalue()), {})


if __name__ == "__main__":
    unittest.main()
