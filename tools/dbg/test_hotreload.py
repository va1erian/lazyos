"""`hotreload.with_version` rewrites only the manifest's version."""

import io
import sys
import unittest
import zipfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import hotreload  # noqa: E402


def package(manifest: bytes) -> bytes:
    out = io.BytesIO()
    with zipfile.ZipFile(out, "w") as z:
        z.writestr("manifest.toml", manifest, compress_type=zipfile.ZIP_DEFLATED)
        z.writestr("bin/app.elf", b"\x7fELF", compress_type=zipfile.ZIP_DEFLATED)
        z.writestr("icons/app-16.png", b"\x89PNG", compress_type=zipfile.ZIP_STORED)
    return out.getvalue()


class WithVersion(unittest.TestCase):
    def test_replaces_only_the_version(self):
        old = package(b'[package]\nname = "x"\nversion = "0.1.0"\n[entry]\nbin = "bin/app.elf"\n')
        new = zipfile.ZipFile(io.BytesIO(hotreload.with_version(old, "0.1.7")))
        self.assertEqual(new.read("manifest.toml"),
                         b'[package]\nname = "x"\nversion = "0.1.7"\n[entry]\nbin = "bin/app.elf"\n')
        self.assertEqual(new.read("bin/app.elf"), b"\x7fELF")
        self.assertEqual(new.getinfo("icons/app-16.png").compress_type, zipfile.ZIP_STORED)
        self.assertEqual(new.getinfo("manifest.toml").compress_type, zipfile.ZIP_DEFLATED)

    def test_a_manifest_without_a_version_is_refused(self):
        with self.assertRaises(ValueError):
            hotreload.with_version(package(b'[package]\nname = "x"\n'), "0.1.7")


if __name__ == "__main__":
    unittest.main()
