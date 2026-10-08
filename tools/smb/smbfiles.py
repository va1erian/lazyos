#!/usr/bin/env python3
"""The harness server's file commands over a real directory: CREATE, CLOSE,
READ, WRITE, FLUSH, QUERY_DIRECTORY, QUERY_INFO and SET_INFO.

Every path is resolved under the served root; a name with `..` or a path that
leaves the root is `OBJECT_NAME_INVALID`. Each handler returns `(status, body,
detail)`, the detail being what the judge reads in the event log.
"""

from __future__ import annotations

import os
import shutil
import stat
import struct
from dataclasses import dataclass, field
from pathlib import Path

import smbproto as p

ATTR_DIRECTORY, ATTR_ARCHIVE = 0x10, 0x20
OPEN, CREATE_NEW, OPEN_IF, OVERWRITE, OVERWRITE_IF, SUPERSEDE = 1, 2, 3, 4, 5, 0
OPT_DIRECTORY, OPT_NON_DIRECTORY = 0x1, 0x40


@dataclass
class Handle:
    path: Path
    name: str
    is_dir: bool
    delete: bool = False
    listing: list[str] | None = None
    cursor: int = 0
    writes: list[tuple[int, int]] = field(default_factory=list)


def _times(info: os.stat_result) -> bytes:
    def ft(t: float) -> int:
        return p.filetime(t)

    return struct.pack("<QQQQ", ft(info.st_ctime), ft(info.st_atime), ft(info.st_mtime), ft(info.st_mtime))


def _attributes(info: os.stat_result) -> int:
    return ATTR_DIRECTORY if stat.S_ISDIR(info.st_mode) else ATTR_ARCHIVE


def _sizes(info: os.stat_result) -> tuple[int, int]:
    size = 0 if stat.S_ISDIR(info.st_mode) else info.st_size
    return (size + 4095) // 4096 * 4096, size


class FileOps:
    def __init__(self, root: Path) -> None:
        self.root = root
        self.handles: dict[int, Handle] = {}
        self.next_id = 1

    def close_all(self) -> None:
        for fid in list(self.handles):
            self._close(fid)

    def resolve(self, name: str) -> Path | None:
        parts = [x for x in name.replace("\\", "/").split("/") if x]
        if any(x in (".", "..") or ":" in x for x in parts):
            return None
        path = self.root.joinpath(*parts)
        real = path.resolve()
        return path if real == self.root or self.root in real.parents else None

    def dispatch(self, command: int, message: bytes, body: bytes) -> tuple[int, bytes, str]:
        handler = {p.CREATE: self.create, p.CLOSE: self.close, p.READ: self.read, p.WRITE: self.write,
                   p.FLUSH: self.flush, p.QUERY_DIRECTORY: self.query_directory,
                   p.QUERY_INFO: self.query_info, p.SET_INFO: self.set_info}.get(command)
        if handler is None:
            return p.NOT_SUPPORTED, b"", "unsupported"
        try:
            return handler(message, body)
        except (KeyError, struct.error):
            return p.INVALID_PARAMETER, b"", "bad request"
        except FileNotFoundError:
            return p.OBJECT_NAME_NOT_FOUND, b"", "not found"
        except OSError as error:
            return p.ACCESS_DENIED, b"", f"os error {error.errno}"

    def _handle(self, body: bytes, at: int) -> tuple[int, Handle]:
        fid = struct.unpack_from("<Q", body, at)[0]
        return fid, self.handles[fid]

    def create(self, message: bytes, body: bytes) -> tuple[int, bytes, str]:
        disposition, options, name_off, name_len = struct.unpack_from("<IIHH", body, 36)
        name = message[name_off:name_off + name_len].decode("utf-16-le") if name_len else ""
        path = self.resolve(name)
        if path is None:
            return p.OBJECT_NAME_INVALID, b"", f"refused {name!r}"
        if not path.parent.is_dir():
            return p.OBJECT_PATH_NOT_FOUND, b"", f"no parent {name}"
        exists = path.exists()
        action = 1
        want_dir = bool(options & OPT_DIRECTORY)
        if disposition == OPEN and not exists:
            return p.OBJECT_NAME_NOT_FOUND, b"", f"missing {name}"
        if disposition == CREATE_NEW and exists:
            return p.OBJECT_NAME_COLLISION, b"", f"exists {name}"
        if not exists:
            if want_dir:
                path.mkdir()
            else:
                path.write_bytes(b"")
            action = 2
        elif disposition in (OVERWRITE, OVERWRITE_IF, SUPERSEDE):
            if path.is_dir():
                return p.FILE_IS_A_DIRECTORY, b"", f"dir {name}"
            path.write_bytes(b"")
            action = 3
        is_dir = path.is_dir()
        if want_dir and not is_dir:
            return p.NOT_A_DIRECTORY, b"", f"not dir {name}"
        if options & OPT_NON_DIRECTORY and is_dir:
            return p.FILE_IS_A_DIRECTORY, b"", f"dir {name}"
        fid = self.next_id
        self.next_id += 1
        self.handles[fid] = Handle(path, name.replace("\\", "/"), is_dir)
        info = path.stat()
        allocation, size = _sizes(info)
        reply = struct.pack("<HBBI", 89, 0, 0, action) + _times(info)
        reply += struct.pack("<QQII", allocation, size, _attributes(info), 0)
        reply += struct.pack("<QQII", fid, fid, 0, 0)
        kind = "dir" if is_dir else "file"
        return p.SUCCESS, reply, f"{name.replace(chr(92), '/') or '/'} {kind} disposition={disposition}"

    def _close(self, fid: int) -> Handle:
        handle = self.handles.pop(fid)
        if handle.delete:
            if handle.is_dir:
                handle.path.rmdir()
            else:
                handle.path.unlink()
        return handle

    def close(self, message: bytes, body: bytes) -> tuple[int, bytes, str]:
        fid, _ = self._handle(body, 8)
        handle = self._close(fid)
        what = "deleted " if handle.delete else ""
        return p.SUCCESS, struct.pack("<HHI", 60, 0, 0) + b"\x00" * 52, f"{what}{handle.name or '/'}"

    def read(self, message: bytes, body: bytes) -> tuple[int, bytes, str]:
        length, offset = struct.unpack_from("<IQ", body, 4)
        _, handle = self._handle(body, 16)
        with open(handle.path, "rb") as f:
            f.seek(offset)
            data = f.read(min(length, 65536))
        if not data:
            return p.END_OF_FILE, b"", f"{handle.name} eof at {offset}"
        reply = struct.pack("<HBBIII", 17, 0x50, 0, len(data), 0, 0) + data
        return p.SUCCESS, reply, f"{handle.name} offset={offset} bytes={len(data)}"

    def write(self, message: bytes, body: bytes) -> tuple[int, bytes, str]:
        data_off, length, offset = struct.unpack_from("<HIQ", body, 2)
        _, handle = self._handle(body, 16)
        data = message[data_off:data_off + length]
        if len(data) != length:
            return p.INVALID_PARAMETER, b"", "short write data"
        with open(handle.path, "r+b") as f:
            f.seek(offset)
            f.write(data)
        handle.writes.append((offset, length))
        return p.SUCCESS, struct.pack("<HHIIHH", 17, 0, length, 0, 0, 0), \
            f"{handle.name} offset={offset} bytes={length}"

    def flush(self, message: bytes, body: bytes) -> tuple[int, bytes, str]:
        _, handle = self._handle(body, 8)
        return p.SUCCESS, struct.pack("<HH", 4, 0), handle.name

    def _entry(self, name: str, path: Path) -> bytes:
        info = path.stat()
        allocation, size = _sizes(info)
        encoded = name.encode("utf-16-le")
        entry = struct.pack("<II", 0, 0) + _times(info) + struct.pack("<QQIII", size, allocation,
                                                                        _attributes(info), len(encoded), 0)
        # Short name length and name (none), reserved, then the file id.
        entry += b"\x00" * 26 + struct.pack("<HQ", 0, info.st_ino & 0xFFFFFFFFFFFFFFFF) + encoded
        return entry + b"\x00" * (-len(entry) % 8)

    def query_directory(self, message: bytes, body: bytes) -> tuple[int, bytes, str]:
        klass, flags = body[2], body[3]
        _, handle = self._handle(body, 8)
        name_off, name_len, out_len = struct.unpack_from("<HHI", body, 24)
        if klass != 37:
            return p.NOT_SUPPORTED, b"", f"class {klass}"
        pattern = message[name_off:name_off + name_len].decode("utf-16-le")
        if handle.listing is None or flags & 0x01:
            names = sorted(os.listdir(handle.path))
            if pattern != "*":
                names = [n for n in names if n == pattern]
            handle.listing, handle.cursor = [".", ".."] + names, 0
        out = b""
        last = 0
        while handle.cursor < len(handle.listing):
            name = handle.listing[handle.cursor]
            target = handle.path if name in (".", "..") else handle.path / name
            entry = self._entry(name, target)
            if len(out) + len(entry) > out_len:
                break
            if out:
                out = out[:last] + struct.pack("<I", len(out) - last) + out[last + 4:]
            last = len(out)
            out += entry
            handle.cursor += 1
        if not out:
            return p.NO_MORE_FILES, b"", f"{handle.name or '/'} end"
        reply = struct.pack("<HHI", 9, p.HEADER + 8, len(out)) + out
        return p.SUCCESS, reply, f"{handle.name or '/'} entries"

    def query_info(self, message: bytes, body: bytes) -> tuple[int, bytes, str]:
        kind, klass, out_len = body[2], body[3], struct.unpack_from("<I", body, 4)[0]
        _, handle = self._handle(body, 24)
        info = handle.path.stat()
        if kind == 2 and klass == 7:
            usage = shutil.disk_usage(self.root)
            data = struct.pack("<QQQII", usage.total // 4096, usage.free // 4096, usage.free // 4096, 8, 512)
        elif kind == 1 and klass == 34:
            allocation, size = _sizes(info)
            data = _times(info) + struct.pack("<QQII", allocation, size, _attributes(info), 0)
        else:
            return p.NOT_SUPPORTED, b"", f"info {kind}/{klass}"
        if len(data) > out_len:
            return p.INVALID_PARAMETER, b"", "buffer too small"
        return p.SUCCESS, struct.pack("<HHI", 9, p.HEADER + 8, len(data)) + data, f"info {kind}/{klass}"

    def set_info(self, message: bytes, body: bytes) -> tuple[int, bytes, str]:
        kind, klass, length, offset = body[2], body[3], *struct.unpack_from("<IH", body, 4)
        _, handle = self._handle(body, 16)
        data = message[offset:offset + length]
        if kind != 1:
            return p.NOT_SUPPORTED, b"", f"set {kind}/{klass}"
        if klass == 13:
            if data[:1] == b"\x01" and handle.is_dir and any(handle.path.iterdir()):
                return p.DIRECTORY_NOT_EMPTY, b"", f"not empty {handle.name}"
            handle.delete = data[:1] == b"\x01"
            return p.SUCCESS, struct.pack("<H", 2), f"delete {handle.name}"
        if klass == 10:
            replace = data[0] != 0
            name_len = struct.unpack_from("<I", data, 16)[0]
            new = data[20:20 + name_len].decode("utf-16-le")
            target = self.resolve(new)
            if target is None:
                return p.OBJECT_NAME_INVALID, b"", f"refused {new!r}"
            if target.exists() and not replace:
                return p.OBJECT_NAME_COLLISION, b"", f"exists {new}"
            os.replace(handle.path, target)
            old, handle.path, handle.name = handle.name, target, new.replace("\\", "/")
            return p.SUCCESS, struct.pack("<H", 2), f"rename {old} -> {handle.name}"
        if klass == 20:
            size = struct.unpack_from("<Q", data)[0]
            os.truncate(handle.path, size)
            return p.SUCCESS, struct.pack("<H", 2), f"size {handle.name} {size}"
        if klass == 4:
            return p.SUCCESS, struct.pack("<H", 2), f"basic {handle.name}"
        return p.NOT_SUPPORTED, b"", f"set class {klass}"
