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
import re
import sys
import zipfile
from pathlib import Path

try:
    import tomllib
except ModuleNotFoundError as error:  # pragma: no cover - Python < 3.11
    raise SystemExit("tools/pkg/build.py needs Python 3.11+ for tomllib") from error

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

MAX_SYSTEM_NAME = 128
MAX_NAME = 64
MAX_AUTHOR = 128
MAX_DESCRIPTION = 1024
MAX_ARGS = 16
MAX_ARG = 256
MAX_VERB = 16
# Mirrors `lazypkg::MAX_NAME_LEN` and `lazypkg::MAX_MANIFEST`.
MAX_NAME_LEN = 255
MAX_MANIFEST = 1024 * 1024

_SYSTEM_LABEL = re.compile(r"^[a-z0-9](?:[a-z0-9-]*[a-z0-9])?$")
_VERSION = re.compile(r"^[0-9]+\.[0-9]+\.[0-9]+$")
_MIME = re.compile(r"^[a-z0-9.+-]+/[a-z0-9.+-]+$")
_VERB = re.compile(r"^[a-z]+$")
_INTERFACE = re.compile(r"^[a-z0-9]+(?:\.[a-z0-9]+)*\.v[0-9]+$")
_FILE_SEGMENT = re.compile(r"^[A-Za-z0-9_.-]+$")
_TOPIC_SEGMENT = re.compile(r"^[a-z0-9_.-]+$")


class BuildError(Exception):
    """A package could not be built; the message lists every problem."""


def _check_keys(table, allowed, where, problems):
    if not isinstance(table, dict):
        problems.append(f"{where} must be a table")
        return
    for key in table:
        if key not in allowed:
            problems.append(f"{where} has unknown field {key!r}")


def _valid_system_name(name):
    if not isinstance(name, str) or not (0 < len(name.encode("utf-8")) <= MAX_SYSTEM_NAME):
        return False
    labels = name.split(".")
    return len(labels) >= 3 and all(_SYSTEM_LABEL.match(label) for label in labels)


def _valid_version(version):
    if not isinstance(version, str) or not _VERSION.match(version):
        return False
    return all(int(part) < 65536 for part in version.split("."))


def _valid_topic(topic):
    if not isinstance(topic, str):
        return False
    if topic.startswith("publish:"):
        rest = topic[len("publish:"):]
    elif topic.startswith("subscribe:"):
        rest = topic[len("subscribe:"):]
    else:
        return False
    segments = rest.split("/")
    if not rest or not segments:
        return False
    for index, segment in enumerate(segments):
        if segment == "#":
            if index != len(segments) - 1:
                return False
        elif segment == "+":
            continue
        elif not _TOPIC_SEGMENT.match(segment):
            return False
    return True


def _valid_file_rule(rule):
    if not isinstance(rule, str):
        return False
    if rule.startswith("read:"):
        rest = rule[len("read:"):]
    elif rule.startswith("write:"):
        rest = rule[len("write:"):]
    else:
        return False
    if not rest.startswith("/") or rest == "/":
        return False
    for segment in rest[1:].split("/"):
        if not segment or segment == "..":
            return False
        if segment != "*" and not _FILE_SEGMENT.match(segment):
            return False
    return True


def validate_manifest(manifest):
    """Return every problem in a parsed manifest (empty means valid)."""
    problems = []
    _check_keys(manifest, {"app", "entry", "mime", "permissions"}, "manifest", problems)
    app = manifest.get("app")
    _check_keys(app, {"name", "system_name", "author", "version", "description"}, "app", problems)
    if not isinstance(app, dict):
        return problems + ["app must be a table"]
    name = app.get("name")
    if not isinstance(name, str) or not (1 <= len(name) <= MAX_NAME):
        problems.append(f"app.name must be 1..{MAX_NAME} characters")
    elif any(ord(char) < 0x20 or 0x7F <= ord(char) <= 0x9F for char in name):
        problems.append("app.name must not contain control characters")
    if not _valid_system_name(app.get("system_name")):
        problems.append(f"app.system_name {app.get('system_name')!r} is not a reverse-DNS name")
    author = app.get("author")
    if not isinstance(author, str) or not (1 <= len(author) <= MAX_AUTHOR):
        problems.append(f"app.author must be 1..{MAX_AUTHOR} characters")
    if not _valid_version(app.get("version")):
        problems.append(f"app.version {app.get('version')!r} must be MAJOR.MINOR.PATCH below 65536")
    description = app.get("description")
    if description is not None and (not isinstance(description, str) or len(description) > MAX_DESCRIPTION):
        problems.append(f"app.description must be at most {MAX_DESCRIPTION} characters")

    entry = manifest.get("entry")
    _check_keys(entry, {"binary", "args", "abi"}, "entry", problems)
    if not isinstance(entry, dict):
        return problems + ["entry must be a table"]
    binary = entry.get("binary")
    if not isinstance(binary, str) or not binary.endswith(".elf"):
        problems.append(f"entry.binary {binary!r} must name a .elf file")
    abi = entry.get("abi")
    if abi is not None and abi not in ("native", "linux"):
        problems.append(f'entry.abi {abi!r} must be "native" or "linux"')
    args = entry.get("args", [])
    if not isinstance(args, list) or len(args) > MAX_ARGS:
        problems.append(f"entry.args may hold at most {MAX_ARGS} items")
    else:
        for arg in args:
            if not isinstance(arg, str) or len(arg.encode("utf-8")) > MAX_ARG:
                problems.append(f"entry.args items must be at most {MAX_ARG} bytes")

    mime = manifest.get("mime", [])
    if not isinstance(mime, list):
        problems.append("mime must be an array of tables")
    else:
        for index, handler in enumerate(mime):
            _check_keys(handler, {"type", "verbs", "icon"}, f"mime[{index}]", problems)
            if not isinstance(handler, dict):
                continue
            mime_type = handler.get("type")
            if not isinstance(mime_type, str) or not _MIME.match(mime_type):
                problems.append(f"mime[{index}].type must be type/subtype")
            verbs = handler.get("verbs")
            if not isinstance(verbs, list) or not verbs:
                problems.append(f"mime[{index}].verbs must not be empty")
            else:
                for verb in verbs:
                    if not isinstance(verb, str) or not (1 <= len(verb) <= MAX_VERB) or not _VERB.match(verb):
                        problems.append(f"mime[{index}] verb {verb!r} must be 1..{MAX_VERB} lowercase letters")
            icon = handler.get("icon")
            if icon is not None and (not isinstance(icon, str) or not icon.startswith("icons/") or ".." in icon):
                problems.append(f"mime[{index}].icon must be an icons/ prefix")

    permissions = manifest.get("permissions", {})
    _check_keys(permissions, {"interfaces", "topics", "files", "network"}, "permissions", problems)
    if isinstance(permissions, dict):
        for interface in _list_field(permissions, "interfaces", "permissions", problems):
            if not isinstance(interface, str) or not _INTERFACE.match(interface):
                problems.append(f"permissions.interfaces entry {interface!r} is not name.vN")
        for topic in _list_field(permissions, "topics", "permissions", problems):
            if not _valid_topic(topic):
                problems.append(f"permissions.topics entry {topic!r} is not a publish:/subscribe: pattern")
        for rule in _list_field(permissions, "files", "permissions", problems):
            if not _valid_file_rule(rule):
                problems.append(f"permissions.files entry {rule!r} is not a read:/write: absolute path")
        network = permissions.get("network", [])
        if network not in ([], ["outbound"]):
            problems.append('permissions.network must be empty or exactly ["outbound"]')
    return problems


def _list_field(table, key, where, problems):
    """`table[key]` when it is a list, else record a problem and yield nothing."""
    value = table.get(key, [])
    if isinstance(value, list):
        return value
    problems.append(f"{where}.{key} must be an array")
    return []


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
