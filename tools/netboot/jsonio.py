"""Small JSON files that are written whole and replaced atomically.

`channels.json`, `machines.json` and the nonce list are read on every use
(so an administrator's edit of the file takes effect at once) and written
through a temporary file and `os.replace`, so a crash leaves the old file or
the new one, never half of one.
"""

from __future__ import annotations

import json
import os
import tempfile
from pathlib import Path


def read_json(path: Path, default):
    """The parsed file, or `default` when it is missing or unreadable."""
    try:
        return json.loads(Path(path).read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return default


def write_json(path: Path, value) -> None:
    """Replace `path` with `value` (indented, trailing newline) atomically."""
    path = Path(path)
    fd, tmp = tempfile.mkstemp(prefix=path.name + ".", suffix=".tmp", dir=path.parent)
    try:
        with os.fdopen(fd, "w", encoding="utf-8", newline="\n") as f:
            json.dump(value, f, indent=2, sort_keys=True)
            f.write("\n")
            f.flush()
            os.fsync(f.fileno())
        os.replace(tmp, path)
    except BaseException:
        try:
            os.unlink(tmp)
        except OSError:
            pass
        raise


def fsync_dir(path: Path) -> None:
    """Make a rename inside `path` durable (a no-op where directories cannot be opened)."""
    try:
        fd = os.open(path, os.O_RDONLY)
    except OSError:
        return
    try:
        os.fsync(fd)
    except OSError:
        pass
    finally:
        os.close(fd)
