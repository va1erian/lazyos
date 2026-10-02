"""The `manifest.toml` rules of a LazyOS package, as the Rust reader has them.

`libs/lazypkg` (`validate.rs`, `grammar.rs`, `files.rs`, `version.rs`) is the
reference; this module checks the same rules on the host so a mistake is caught
before an archive is written. Both run the shared cases in
`libs/lazypkg/tests/cases/manifest.toml`, so the two cannot drift apart
silently. See `docs/packages.md`, section 3.
"""

from __future__ import annotations

import functools
import re

MAX_SYSTEM_NAME = 128
MAX_NAME = 64
MAX_AUTHOR = 128
MAX_DESCRIPTION = 1024
MAX_ARGS = 16
MAX_ARG = 256
MAX_VERB = 16

# `lazypkg::Category::ALL`, in menu order; the first is the default.
CATEGORIES = ("accessories", "development", "graphics", "internet", "office", "system", "utilities")
DEFAULT_CATEGORY = CATEGORIES[0]

# `lazypkg::HOME_VAR`: a files rule may start with it, and only there.
HOME_VAR = "$HOME"
# F5 cleanup switch, mirroring `files::REJECT_ABSOLUTE_HOME` in libs/lazypkg:
# when True, an absolute path inside a home directory is refused with a pointer
# to `$HOME`. Flip both together.
REJECT_ABSOLUTE_HOME = False
# `fhs::state::HOME_ROOT` and `fhs::mount::HOME`.
HOME_ROOTS = ("/data/home", "/home")

_SYSTEM_LABEL = re.compile(r"[a-z0-9](?:[a-z0-9-]*[a-z0-9])?")
_MIME = re.compile(r"[a-z0-9.+-]+/[a-z0-9.+-]+")
_VERB = re.compile(r"[a-z]+")
_INTERFACE = re.compile(r"[a-z0-9]+(?:\.[a-z0-9]+)*\.v[0-9]+")
_FILE_SEGMENT = re.compile(r"[A-Za-z0-9_.-]+")
_TOPIC_SEGMENT = re.compile(r"[a-z0-9_.-]+")

# --- Versions (`lazypkg::Version`) -------------------------------------------

MAX_VERSION_LEN = 64
_NUMBER = re.compile(r"0|[1-9][0-9]*")
_IDENT = re.compile(r"[0-9A-Za-z-]+")
_DIGITS = re.compile(r"[0-9]+")

# The `Display` of each `lazypkg::VersionError`, word for word.
VERSION_LENGTH = "is empty or longer than 64 bytes"
VERSION_COMPONENTS = "must have two to four numbers"
VERSION_NUMBER = "has a number that is not decimal below 65536 without a leading zero"
VERSION_PRERELEASE = "has a pre-release part that is not dot-separated [0-9A-Za-z-] identifiers"


class VersionError(ValueError):
    """`text` is not a version; the message is the reason."""


def _identifier_key(ident):
    # Semver 11.4: numeric identifiers numerically, before alphanumeric ones.
    return (0, int(ident), "") if _DIGITS.fullmatch(ident) else (1, 0, ident)


@functools.total_ordering
class Version:
    """A package version, ordered like `lazypkg::Version` (1.0 == 1.0.0)."""

    def __init__(self, text):
        if not isinstance(text, str) or not (0 < len(text.encode("utf-8")) <= MAX_VERSION_LEN):
            raise VersionError(VERSION_LENGTH)
        core_text, dash, pre = text.partition("-")
        parts = core_text.split(".")
        if not 2 <= len(parts) <= 4:
            raise VersionError(VERSION_COMPONENTS)
        if not all(_NUMBER.fullmatch(part) and int(part) < 65536 for part in parts):
            raise VersionError(VERSION_NUMBER)
        idents = pre.split(".") if dash else []
        for ident in idents:
            if not _IDENT.fullmatch(ident) or (_DIGITS.fullmatch(ident) and not _NUMBER.fullmatch(ident)):
                raise VersionError(VERSION_PRERELEASE)
        self.text = text
        self.core = tuple(int(part) for part in parts) + (0,) * (4 - len(parts))
        self.prerelease = pre if dash else None
        # A release sorts after every pre-release of the same core.
        # Tuples compare like semver identifier lists: a shorter prefix is lower.
        pre_key = (0, tuple(_identifier_key(ident) for ident in idents)) if dash else (1, ())
        self._key = (self.core, pre_key)

    def __eq__(self, other):
        return isinstance(other, Version) and self._key == other._key

    def __lt__(self, other):
        return self._key < other._key

    def __hash__(self):
        return hash(self._key)

    def __str__(self):
        return self.text

    def __repr__(self):
        return f"Version({self.text!r})"


def version_problem(text):
    """Why `text` is not a version, or None."""
    try:
        Version(text)
    except VersionError as error:
        return str(error)
    return None


# --- Field grammars (`grammar.rs`, `files.rs`) --------------------------------


def _valid_system_name(name):
    if not isinstance(name, str) or not (0 < len(name.encode("utf-8")) <= MAX_SYSTEM_NAME):
        return False
    labels = name.split(".")
    return len(labels) >= 3 and all(_SYSTEM_LABEL.fullmatch(label) for label in labels)


def _valid_topic(topic):
    if not isinstance(topic, str):
        return False
    if topic.startswith("publish:"):
        rest = topic[len("publish:"):]
    elif topic.startswith("subscribe:"):
        rest = topic[len("subscribe:"):]
    else:
        return False
    if not rest:
        return False
    segments = rest.split("/")
    for index, segment in enumerate(segments):
        if segment == "#":
            if index != len(segments) - 1:
                return False
        elif segment == "+":
            continue
        elif not _TOPIC_SEGMENT.fullmatch(segment):
            return False
    return True


def is_absolute_home(path):
    """Whether `path` is a home root or inside one (`files::is_absolute_home`)."""
    return any(path == root or path.startswith(root + "/") for root in HOME_ROOTS)


def file_rule_problem(rule):
    """Why the files rule `rule` is refused, or None (`files::check_rule`)."""
    shape = f"permissions.files entry {rule!r} is not a read:/write: absolute or $HOME/ path"
    if not isinstance(rule, str):
        return shape
    for verb in ("read:", "write:"):
        if rule.startswith(verb):
            path = rule[len(verb):]
            break
    else:
        return shape
    relative = path.startswith(HOME_VAR)
    rest = path[len(HOME_VAR):] if relative else path
    if not rest.startswith("/"):
        return shape
    rest = rest[1:]
    if HOME_VAR in rest:
        return f"permissions.files entry {rule!r} may use $HOME only as its first segment"
    for segment in rest.split("/"):
        if not segment or segment == "..":
            return shape
        if segment != "*" and not _FILE_SEGMENT.fullmatch(segment):
            return shape
    if REJECT_ABSOLUTE_HOME and not relative and is_absolute_home(path):
        return f"permissions.files entry {rule!r} names a home directory; write it as $HOME/..."
    return None


# --- The manifest (`validate.rs`) ---------------------------------------------


def _check_keys(table, allowed, where, problems):
    if not isinstance(table, dict):
        problems.append(f"{where} must be a table")
        return
    for key in table:
        if key not in allowed:
            problems.append(f"{where} has unknown field {key!r}")


def _list_field(table, key, where, problems):
    """`table[key]` when it is a list, else record a problem and yield nothing."""
    value = table.get(key, [])
    if isinstance(value, list):
        return value
    problems.append(f"{where}.{key} must be an array")
    return []


def _check_app(app, problems):
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
    reason = version_problem(app.get("version"))
    if reason is not None:
        problems.append(f"app.version {app.get('version')!r} {reason}")
    description = app.get("description")
    if description is not None and (not isinstance(description, str) or len(description) > MAX_DESCRIPTION):
        problems.append(f"app.description must be at most {MAX_DESCRIPTION} characters")
    category = app.get("category")
    if category is not None and category not in CATEGORIES:
        problems.append(f"app.category {category!r} must be one of {', '.join(CATEGORIES)}")


def _check_entry(entry, problems):
    binary = entry.get("binary")
    if not isinstance(binary, str) or not binary.endswith(".elf"):
        problems.append(f"entry.binary {binary!r} must name a .elf file")
    abi = entry.get("abi")
    if abi is not None and abi not in ("native", "linux"):
        problems.append(f'entry.abi {abi!r} must be "native" or "linux"')
    autostart = entry.get("autostart", False)
    if not isinstance(autostart, bool):
        problems.append(f"entry.autostart {autostart!r} must be true or false")
    args = entry.get("args", [])
    if not isinstance(args, list) or len(args) > MAX_ARGS:
        problems.append(f"entry.args may hold at most {MAX_ARGS} items")
    else:
        for arg in args:
            if not isinstance(arg, str) or len(arg.encode("utf-8")) > MAX_ARG:
                problems.append(f"entry.args items must be at most {MAX_ARG} bytes")


def _check_mime(mime, problems):
    if not isinstance(mime, list):
        problems.append("mime must be an array of tables")
        return
    for index, handler in enumerate(mime):
        _check_keys(handler, {"type", "verbs", "icon"}, f"mime[{index}]", problems)
        if not isinstance(handler, dict):
            continue
        mime_type = handler.get("type")
        if not isinstance(mime_type, str) or not _MIME.fullmatch(mime_type):
            problems.append(f"mime[{index}].type must be type/subtype")
        verbs = handler.get("verbs")
        if not isinstance(verbs, list) or not verbs:
            problems.append(f"mime[{index}].verbs must not be empty")
        else:
            for verb in verbs:
                if not isinstance(verb, str) or not (1 <= len(verb) <= MAX_VERB) or not _VERB.fullmatch(verb):
                    problems.append(f"mime[{index}] verb {verb!r} must be 1..{MAX_VERB} lowercase letters")
        icon = handler.get("icon")
        if icon is not None and (not isinstance(icon, str) or not icon.startswith("icons/") or ".." in icon):
            problems.append(f"mime[{index}].icon must be an icons/ prefix")


def _check_permissions(permissions, problems):
    _check_keys(permissions, {"interfaces", "topics", "files", "network"}, "permissions", problems)
    if not isinstance(permissions, dict):
        return
    for interface in _list_field(permissions, "interfaces", "permissions", problems):
        if not isinstance(interface, str) or not _INTERFACE.fullmatch(interface):
            problems.append(f"permissions.interfaces entry {interface!r} is not name.vN")
    for topic in _list_field(permissions, "topics", "permissions", problems):
        if not _valid_topic(topic):
            problems.append(f"permissions.topics entry {topic!r} is not a publish:/subscribe: pattern")
    for rule in _list_field(permissions, "files", "permissions", problems):
        problem = file_rule_problem(rule)
        if problem is not None:
            problems.append(problem)
    if permissions.get("network", []) not in ([], ["outbound"]):
        problems.append('permissions.network must be empty or exactly ["outbound"]')


def validate_manifest(manifest):
    """Return every problem in a parsed manifest (empty means valid)."""
    problems = []
    _check_keys(manifest, {"app", "entry", "mime", "permissions"}, "manifest", problems)
    app = manifest.get("app")
    _check_keys(app, {"name", "system_name", "author", "version", "description", "category"}, "app", problems)
    if not isinstance(app, dict):
        return problems + ["app must be a table"]
    _check_app(app, problems)
    entry = manifest.get("entry")
    _check_keys(entry, {"binary", "args", "abi", "autostart"}, "entry", problems)
    if not isinstance(entry, dict):
        return problems + ["entry must be a table"]
    _check_entry(entry, problems)
    _check_mime(manifest.get("mime", []), problems)
    _check_permissions(manifest.get("permissions", {}), problems)
    return problems
