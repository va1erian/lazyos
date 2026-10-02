#!/usr/bin/env python3
"""Build a LazyOS application package (`.lzp`) from a source directory.

The source tree is the layout described in `docs/packages.md`:

    manifest.toml
    bin/<name>.elf
    icons/app-16.png  icons/app-32.png  icons/app-128.png
    icons/<slug>-{16,32,128}.png     (optional)
    idl/*.midl  docs/*.md  resources/**

This tool runs the same structural and manifest checks the Rust reader
(`libs/lazypkg`) enforces, so a mistake is caught on the host instead of on the
OS. `.png` files are stored; everything else is deflated. The output is
`<system_name>-<version>.lzp`.

    python tools/pkg/build.py path/to/tree
    python tools/pkg/build.py path/to/tree --out dist
"""

from __future__ import annotations

import argparse
import sys
import zipfile
from pathlib import Path

try:
    import tomllib
except ModuleNotFoundError as error:  # pragma: no cover - Python < 3.11
    raise SystemExit("tools/pkg/build.py needs Python 3.11+ for tomllib") from error

sys.path.insert(0, str(Path(__file__).resolve().parent))
# The manifest rules live in their own module; `validate_manifest` is part of
# this tool's interface too (tests and other tools call `build.validate_manifest`).
from pkgmanifest import Version, validate_manifest  # noqa: E402,F401

# Top-level directories a package may contain, and the extension files inside
# each must use (`resources/` is unconstrained).
ALLOWED_DIRS = {
    "bin": ".elf",
    "icons": ".png",
    "idl": ".midl",
    "docs": ".md",
    "resources": None,
}
REQUIRED_ICONS = ["icons/app-16.png", "icons/app-32.png", "icons/app-128.png"]
PNG_SIGNATURE = b"\x89PNG\r\n\x1a\n"

# Mirrors `lazypkg::MAX_NAME_LEN` and `lazypkg::MAX_MANIFEST`.
MAX_NAME_LEN = 255
MAX_MANIFEST = 1024 * 1024


class BuildError(Exception):
    """A package could not be built; the message lists every problem."""


def entry_name_problem(name):
    """Why the reader (`libs/lazypkg/src/path.rs`) would reject `name`, or None."""
    if not name:
        return "is empty"
    if len(name.encode("utf-8")) > MAX_NAME_LEN:
        return f"is longer than {MAX_NAME_LEN} bytes"
    if "\0" in name:
        return "contains a NUL byte"
    if any(ord(ch) < 0x20 or ord(ch) == 0x7F for ch in name):
        return "contains a control character"
    if name.startswith(("/", "\\")):
        return "is absolute"
    if "\\" in name:
        return "contains a backslash"
    if len(name) >= 2 and name[0].isascii() and name[0].isalpha() and name[1] == ":":
        return "has a drive letter"
    for component in name.split("/"):
        if component in ("", ".", ".."):
            return "has an empty, `.` or `..` path component"
    return None


def collect_files(root):
    """Map relative POSIX paths to files under `root`."""
    files = {}
    for path in sorted(root.rglob("*")):
        if path.is_file():
            files[path.relative_to(root).as_posix()] = path
    return files


def validate_layout(files):
    """Return every layout problem for the collected file names."""
    problems = []
    if "manifest.toml" not in files:
        problems.append("manifest.toml is missing")
    for name in files:
        reason = entry_name_problem(name)
        if reason is not None:
            problems.append(f"entry {name!r} {reason}")
            continue
        if name == "manifest.toml":
            continue
        top = name.split("/", 1)[0]
        if top not in ALLOWED_DIRS:
            problems.append(f"entry {name!r} is not under an allowed top-level directory")
            continue
        extension = ALLOWED_DIRS[top]
        if extension is not None and not name.endswith(extension):
            problems.append(f"entry {name!r} has the wrong file extension (expected {extension})")
    for icon in REQUIRED_ICONS:
        if icon not in files:
            problems.append(f"entry {icon!r} is missing")
        elif not files[icon].read_bytes().startswith(PNG_SIGNATURE):
            problems.append(f"entry {icon!r} is not a PNG")
    return problems


def _check_references(manifest, files, problems):
    entry = manifest.get("entry")
    binary = entry.get("binary") if isinstance(entry, dict) else None
    if isinstance(binary, str) and binary.endswith(".elf") and binary not in files:
        problems.append(f"entry.binary {binary!r} is missing from the package")
    mime = manifest.get("mime", [])
    if not isinstance(mime, list):
        return
    for index, handler in enumerate(mime):
        icon = handler.get("icon") if isinstance(handler, dict) else None
        if isinstance(icon, str) and icon.startswith("icons/") and ".." not in icon:
            for size in ("16", "32", "128"):
                path = f"{icon}-{size}.png"
                if path not in files:
                    problems.append(f"mime[{index}].icon {path!r} is missing from the package")


# The earliest timestamp a zip can hold.
FIXED_TIME = (1980, 1, 1, 0, 0, 0)


def build(root, out_dir):
    """Validate `root` and write `<system_name>-<version>.lzp`; return its path."""
    root = Path(root)
    if not root.is_dir():
        raise BuildError(f"{root} is not a directory")
    try:
        manifest_bytes = (root / "manifest.toml").read_bytes()
    except FileNotFoundError as error:
        raise BuildError("manifest.toml is missing") from error
    # The reader refuses a larger manifest before parsing it; so does the builder.
    if len(manifest_bytes) > MAX_MANIFEST:
        raise BuildError(f"manifest.toml is larger than {MAX_MANIFEST} bytes")
    try:
        manifest = tomllib.loads(manifest_bytes.decode("utf-8-sig"))
    except (tomllib.TOMLDecodeError, UnicodeDecodeError) as error:
        raise BuildError(f"manifest.toml is invalid: {error}") from error

    problems = validate_manifest(manifest)
    files = collect_files(root)
    problems += validate_layout(files)
    _check_references(manifest, files, problems)
    if problems:
        raise BuildError("\n".join(problems))

    system_name = manifest["app"]["system_name"]
    version = manifest["app"]["version"]
    out_dir = Path(out_dir)
    out_dir.mkdir(parents=True, exist_ok=True)
    archive = out_dir / f"{system_name}-{version}.lzp"
    with zipfile.ZipFile(archive, "w") as zf:
        for name, path in sorted(files.items()):
            compression = zipfile.ZIP_STORED if name.endswith(".png") else zipfile.ZIP_DEFLATED
            # A fixed timestamp makes the archive a pure function of the tree, so
            # building the same tree twice gives the same digest (and so the same
            # install directory, which is what "already installed" keys on).
            info = zipfile.ZipInfo(name, date_time=FIXED_TIME)
            info.compress_type = compression
            info.external_attr = 0o644 << 16
            zf.writestr(info, path.read_bytes())
    return archive


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("source", type=Path, help="the package source tree")
    parser.add_argument("--out", type=Path, default=Path("."), help="directory for the .lzp (default: .)")
    args = parser.parse_args(argv)
    try:
        archive = build(args.source, args.out)
    except BuildError as error:
        print(f"error: cannot build package:\n{error}", file=sys.stderr)
        return 1
    print(archive)
    return 0


if __name__ == "__main__":
    sys.exit(main())
