"""What a name may be (docs/netboot-plan.md section 5), in one place.

Every name that reaches the file system (a channel, a build id, a file of a
build, a machine) is checked here first. The rule is the whitelist
`[A-Za-z0-9._-]`, no leading dot, so `..`, `a/b`, `C:\\x`, an absolute path
and a NUL can never name anything; callers split the URL path into segments
*before* decoding, so an escaped `%2F` ends up as a `/` in a segment and is
refused as well.
"""

from __future__ import annotations

import re

#: `<yyyymmdd-HHMMSS>-<rev8>` with an optional `-dirty` suffix.
BUILD_ID = re.compile(r"^\d{8}-\d{6}-[0-9a-f]{8}(-dirty)?\Z")
_NAME = re.compile(r"^[A-Za-z0-9._-]{1,128}\Z")
_MAC = re.compile(r"^[0-9a-f]{2}([:-][0-9a-f]{2}){5}\Z")
HEX64 = re.compile(r"^[0-9a-f]{64}\Z")
HEX128 = re.compile(r"^[0-9a-f]{128}\Z")
NONCE = re.compile(r"^[0-9a-f]{32}\Z")


def is_name(value: object) -> bool:
    """A plain name: whitelist characters, not hidden, not `.` or `..`."""
    return isinstance(value, str) and bool(_NAME.match(value)) and not value.startswith(".")


def is_build_id(value: object) -> bool:
    return isinstance(value, str) and bool(BUILD_ID.match(value))


def normalize_mac(value: object) -> str | None:
    """`AA-BB-..` or `aa:bb:..` as lower-case colon form; None when it is no MAC."""
    if not isinstance(value, str):
        return None
    value = value.strip().lower()
    if not _MAC.match(value):
        return None
    return value.replace("-", ":")
