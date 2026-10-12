"""The registry's on-disk layout and the operations on builds (docs/netboot-plan.md section 5).

    <root>/
      builds/<channel>/<id>/{kernel-x86_64,ramdisk,bootx64.efi,boot.json,manifest.json,manifest.sig}
      builds/<channel>/<id>.incoming/     an upload in progress
      channels.json   {"dev": {"keep": 10, "pinned": ["<id>", ...]}}
      machines.json   (machines.py)
      keys.json       the public keys allowed to write
      nonces.json     the signed-request nonces seen recently
      access.log

Builds are immutable and named: a build exists in exactly one channel under
one id, appears all at once (one `rename` of a verified `.incoming`
directory) and is never overwritten. The store never reads a byte of a
ramdisk except to hash it when a build is committed. Errors the HTTP layer
should show are `StoreError(status, message)`.
"""

from __future__ import annotations

import hashlib
import json
import os
import shutil
import threading
import time
from pathlib import Path

import auth
import ed25519
import jsonio
import keys
import names

MANIFEST = "manifest.json"
SIGNATURE = "manifest.sig"
MAX_MANIFEST = 1 << 20
MAX_SIGNATURE = 4096
#: Most files one upload may hold (a build has four).
MAX_UPLOAD_FILES = 64
#: A leftover `.incoming` directory older than this is removed (seconds).
STALE_INCOMING_S = 24 * 3600
CHUNK = 1 << 20


class StoreError(Exception):
    def __init__(self, status: int, message: str):
        super().__init__(message)
        self.status, self.message = status, message


def iso(seconds: float) -> str:
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(seconds))


def _check(ok: bool, what: str, value) -> None:
    if not ok:
        raise StoreError(400, f"bad {what}: {str(value)[:80]!r}")


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with open(path, "rb") as f:
        while chunk := f.read(CHUNK):
            digest.update(chunk)
    return digest.hexdigest()


class Store:
    def __init__(self, root: Path, keys_path: Path | None = None, keep: int = 10, clock=time.time):
        self.root = Path(root)
        self.keys_path = Path(keys_path) if keys_path else self.root / "keys.json"
        self.keep = keep
        self.clock = clock
        self.lock = threading.RLock()  # guards the JSON files and the rename into place
        self.nonces = auth.NonceList(self.root / "nonces.json", clock)

    # -- layout ---------------------------------------------------------

    @property
    def builds_dir(self) -> Path:
        return self.root / "builds"

    @property
    def channels_path(self) -> Path:
        return self.root / "channels.json"

    def build_dir(self, channel: str, build_id: str) -> Path:
        return self.builds_dir / channel / build_id

    def incoming_dir(self, channel: str, build_id: str) -> Path:
        return self.builds_dir / channel / (build_id + ".incoming")

    def init_layout(self) -> list[str]:
        """Create what a first run on an empty root needs; return the lines to log."""
        notes = []
        self.root.mkdir(parents=True, exist_ok=True)
        self.builds_dir.mkdir(exist_ok=True)
        for path, empty in ((self.channels_path, {}), (self.root / "machines.json", {})):
            if not path.exists():
                jsonio.write_json(path, empty)
                notes.append(f"created {path}")
        if not self.keys_path.exists():
            self.keys_path.parent.mkdir(parents=True, exist_ok=True)
            jsonio.write_json(self.keys_path, {"keys": []})
            notes.append(f"created {self.keys_path}")
        if not keys.load_keys(self.keys_path):
            notes.append(f"no key may push yet: add an entry to {self.keys_path} "
                         "(push.py --register-key prints it)")
        self.sweep_incoming()
        return notes

    def known_keys(self) -> dict[str, bytes]:
        return keys.load_keys(self.keys_path)

    # -- reading --------------------------------------------------------

    def channel_meta(self) -> dict:
        doc = jsonio.read_json(self.channels_path, {})
        return doc if isinstance(doc, dict) else {}

    def settings(self, channel: str) -> tuple[int, list[str]]:
        """`(keep, pinned ids)` of a channel; `keep` falls back to the registry's default."""
        meta = self.channel_meta().get(channel)
        meta = meta if isinstance(meta, dict) else {}
        keep = meta.get("keep")
        pinned = meta.get("pinned")
        return (keep if isinstance(keep, int) and keep >= 1 else self.keep,
                [p for p in pinned if isinstance(p, str)] if isinstance(pinned, list) else [])

    def channel_names(self) -> list[str]:
        """Channels that exist: a directory under builds/ holding a build, sorted."""
        found = []
        try:
            entries = sorted(self.builds_dir.iterdir())
        except OSError:
            return []
        for entry in entries:
            if names.is_name(entry.name) and entry.is_dir() and self.build_ids(entry.name):
                found.append(entry.name)
        return found

    def build_ids(self, channel: str) -> list[str]:
        """The committed builds of a channel, newest first (ids sort by build time)."""
        try:
            entries = (e for e in (self.builds_dir / channel).iterdir()
                       if names.is_build_id(e.name) and e.is_dir())
        except OSError:
            return []
        return sorted((e.name for e in entries), reverse=True)

    def find_build(self, build_id: str) -> str | None:
        """The channel holding `build_id`, if any."""
        for channel in self.channel_names():
            if (self.build_dir(channel, build_id)).is_dir():
                return channel
        return None

    def load_build(self, channel: str, build_id: str, pinned: list[str] | None = None) -> dict | None:
        """One build as the API shows it: the manifest (parsed and verbatim) and its signature."""
        directory = self.build_dir(channel, build_id)
        try:
            text = (directory / MANIFEST).read_text(encoding="utf-8")
            sig = (directory / SIGNATURE).read_text(encoding="ascii").strip()
            manifest = json.loads(text)
        except (OSError, ValueError):
            return None
        files = manifest.get("files", {}) if isinstance(manifest, dict) else {}
        sizes = [f.get("size", 0) for f in files.values() if isinstance(f, dict)]
        if pinned is None:
            pinned = self.settings(channel)[1]
        return {"id": build_id, "channel": channel, "pinned": build_id in pinned,
                "manifest": manifest, "manifest_text": text, "sig": sig,
                "size": sum(s for s in sizes if isinstance(s, int))}

    def builds(self, channel: str) -> list[dict]:
        pinned = self.settings(channel)[1]
        found = (self.load_build(channel, i, pinned) for i in self.build_ids(channel))
        return [b for b in found if b is not None]

    def file_path(self, channel: str, build_id: str, name: str) -> Path | None:
        """A file of a committed build, or None (also for any name that is not a plain name)."""
        if not (names.is_name(channel) and names.is_build_id(build_id) and names.is_name(name)):
            return None
        path = self.build_dir(channel, build_id) / name
        return path if path.is_file() else None

    # -- uploading and committing ---------------------------------------

    def put_file(self, channel: str, build_id: str, name: str, stream, length: int) -> int:
        """Store `length` bytes of `stream` as one file of the build's incoming directory.

        An upload is opened by its signed manifest: `manifest.json` and
        `manifest.sig` come first and are verified the moment both are there
        (a pair that does not verify deletes the upload). Until then every
        other file is refused, and afterwards only a file the manifest lists,
        with exactly the size it lists, so a host that holds no key cannot
        make the registry store more than a signed manifest allows.
        """
        _check(names.is_name(channel), "channel", channel)
        _check(names.is_build_id(build_id), "build id", build_id)
        _check(names.is_name(name), "file name", name)
        if self.find_build(build_id):
            raise StoreError(409, f"build {build_id} already exists; ids are never overwritten")
        directory = self.incoming_dir(channel, build_id)
        is_manifest = name in (MANIFEST, SIGNATURE)
        if is_manifest:
            limit = MAX_MANIFEST if name == MANIFEST else MAX_SIGNATURE
            if length > limit:
                raise StoreError(413, f"{name} is larger than {limit} bytes")
        else:
            entry = self._opened_manifest(directory, build_id)["files"].get(name)
            if entry is None:
                raise StoreError(400, f"{name} is not a file of the manifest")
            if length != entry["size"]:
                raise StoreError(413, f"{name}: Content-Length {length} != manifest size {entry['size']}")
        directory.mkdir(parents=True, exist_ok=True)
        existing = [p for p in directory.iterdir() if not p.name.startswith(".")]
        if len(existing) >= MAX_UPLOAD_FILES and not (directory / name).exists():
            raise StoreError(400, f"too many files in one upload (limit {MAX_UPLOAD_FILES})")
        try:
            written = self._write_part(directory, name, stream, length)
        except FileNotFoundError:  # a commit claimed (renamed) the directory under this PUT
            raise StoreError(409, "the upload was claimed by a commit while this file was written") from None
        if is_manifest and (directory / MANIFEST).exists() and (directory / SIGNATURE).exists():
            try:
                self._open_manifest(directory, build_id)
            except StoreError:
                shutil.rmtree(directory, ignore_errors=True)
                raise
        return written

    @staticmethod
    def _write_part(directory: Path, name: str, stream, length: int) -> int:
        part = directory / f".{name}.{os.urandom(4).hex()}.part"
        written = 0
        try:
            with open(part, "wb") as f:
                while written < length:
                    chunk = stream.read(min(CHUNK, length - written))
                    if not chunk:
                        raise StoreError(400, f"the body ended after {written} of {length} bytes")
                    f.write(chunk)
                    written += len(chunk)
                f.flush()
                os.fsync(f.fileno())
            os.replace(part, directory / name)
        except BaseException:
            part.unlink(missing_ok=True)
            raise
        return written

    def _open_manifest(self, directory: Path, build_id: str) -> dict:
        """The parsed manifest of an upload whose pair verifies against `keys.json`, else raise."""
        raw = (directory / MANIFEST).read_bytes()
        with open(directory / SIGNATURE, "rb") as f:
            self._signer(raw, f.read(MAX_SIGNATURE))
        manifest = self._parse_manifest(raw)
        if manifest.get("id") != build_id:
            raise StoreError(400, f'{MANIFEST} "id" is not {build_id}')
        return manifest

    def _opened_manifest(self, directory: Path, build_id: str) -> dict:
        if not (directory / MANIFEST).is_file() or not (directory / SIGNATURE).is_file():
            raise StoreError(403, f"upload {MANIFEST} and {SIGNATURE} first: files are accepted only "
                                  "once the signed manifest that lists them is in place")
        return self._open_manifest(directory, build_id)

    def _verify_incoming(self, directory: Path) -> tuple[bytes, str, int]:
        """Check an upload; return `(manifest bytes, signing key id, total size)` or raise."""
        have = {p.name for p in directory.iterdir() if p.is_file() and not p.name.startswith(".")}
        if MANIFEST not in have or SIGNATURE not in have:
            raise StoreError(400, f"{MANIFEST} and {SIGNATURE} must both be uploaded before the commit")
        raw = (directory / MANIFEST).read_bytes()
        if len(raw) > MAX_MANIFEST:
            raise StoreError(400, f"{MANIFEST} is larger than {MAX_MANIFEST} bytes")
        with open(directory / SIGNATURE, "rb") as f:
            signer = self._signer(raw, f.read(MAX_SIGNATURE))
        manifest = self._parse_manifest(raw)
        files = manifest["files"]
        wanted = set(files) | {MANIFEST, SIGNATURE}
        if have - wanted:
            raise StoreError(400, f"files the manifest does not list: {', '.join(sorted(have - wanted))}")
        if wanted - have:
            raise StoreError(400, f"files missing from the upload: {', '.join(sorted(wanted - have))}")
        for name, entry in sorted(files.items()):
            path = directory / name
            if path.stat().st_size != entry["size"]:
                raise StoreError(422, f"{name}: size {path.stat().st_size} != manifest {entry['size']}")
        for name, entry in sorted(files.items()):
            if sha256_file(directory / name) != entry["sha256"]:
                raise StoreError(422, f"{name}: SHA-256 does not match the manifest")
        return raw, signer, sum(e["size"] for e in files.values())

    def _signer(self, manifest: bytes, sig_text: bytes) -> str:
        sig = keys.parse_signature(sig_text)
        if sig is None:
            raise StoreError(400, f"{SIGNATURE} must be 128 lower-case hex characters and a newline")
        known = self.known_keys()
        if not known:
            raise StoreError(403, "no key is registered in keys.json, so no build can be committed")
        for key_id, pub in known.items():
            if ed25519.verify(pub, manifest, sig):
                return key_id
        raise StoreError(403, f"{SIGNATURE} does not verify against any key in keys.json")

    @staticmethod
    def _parse_manifest(raw: bytes) -> dict:
        try:
            manifest = json.loads(raw.decode("utf-8"))
        except ValueError:
            raise StoreError(400, f"{MANIFEST} is not UTF-8 JSON") from None
        if not isinstance(manifest, dict) or not isinstance(manifest.get("files"), dict) \
                or not manifest["files"]:
            raise StoreError(400, f'{MANIFEST} needs a non-empty "files" object')
        for name, entry in manifest["files"].items():
            ok = (names.is_name(name) and name not in (MANIFEST, SIGNATURE) and isinstance(entry, dict)
                  and isinstance(entry.get("size"), int) and not isinstance(entry.get("size"), bool)
                  and entry["size"] >= 0 and isinstance(entry.get("sha256"), str)
                  and bool(names.HEX64.match(entry["sha256"])))
            if not ok:
                raise StoreError(400, f"{MANIFEST}: bad entry for file {name[:40]!r}")
        return manifest

    def commit(self, channel: str, build_id: str) -> dict:
        """Verify the upload and move it into place, or delete it and raise.

        Checks: `manifest.sig` verifies, under a key of `keys.json`, over the
        exact bytes of `manifest.json`; the manifest names this id; the
        uploaded files are exactly the manifest's, with its sizes and SHA-256.
        The same id again with the same manifest is a no-op (a retried push);
        with any other bytes it is refused.

        The commit first claims the upload by renaming `<id>.incoming` to a
        private `<id>.verifying-<8 hex>` under the lock, and verifies that
        directory. A PUT that arrives after the claim finds no incoming
        directory, so (manifest-first) it is refused, and a second commit finds
        nothing to claim: no byte can enter a build after it was hashed.
        """
        _check(names.is_name(channel), "channel", channel)
        _check(names.is_build_id(build_id), "build id", build_id)
        claimed = self._claim(channel, build_id)
        published = False
        try:
            raw, signer, size = self._verify_incoming(claimed)
            if self._parse_manifest(raw).get("id") != build_id:
                raise StoreError(400, f'{MANIFEST} "id" is not {build_id}')
            with self.lock:
                existing = self.find_build(build_id)
                if existing is not None:
                    same = (existing == channel and
                            (self.build_dir(channel, build_id) / MANIFEST).read_bytes() == raw)
                    if not same:
                        raise StoreError(409, f"build {build_id} already exists in channel {existing} "
                                              "with other contents; ids are never overwritten")
                    return {"id": build_id, "channel": channel, "already": True, "key": signer}
                for leftover in claimed.glob(".*"):  # a PUT that was in flight must not ride along
                    leftover.unlink(missing_ok=True)
                os.rename(claimed, self.build_dir(channel, build_id))
                published = True
                jsonio.fsync_dir(self.builds_dir / channel)
                self._register_channel(channel)
        finally:
            if not published:  # refused, a no-op or an error: the claimed upload goes
                shutil.rmtree(claimed, ignore_errors=True)
        return {"id": build_id, "channel": channel, "already": False, "key": signer, "size": size}

    def _claim(self, channel: str, build_id: str) -> Path:
        """Rename the incoming directory to a private one nobody else can write into."""
        with self.lock:
            incoming = self.incoming_dir(channel, build_id)
            if not incoming.is_dir():
                raise StoreError(404, f"nothing uploaded for {channel}/{build_id}, "
                                      "or another commit claimed it")
            claimed = incoming.with_name(f"{build_id}.verifying-{os.urandom(4).hex()}")
            try:
                os.rename(incoming, claimed)
            except OSError as e:  # e.g. Windows, while a PUT still holds a file open
                raise StoreError(409, f"the upload is still being written ({e.strerror})") from e
            return claimed

    def sweep_incoming(self) -> int:
        """Remove uploads nobody finished (incoming or abandoned mid-commit, older than a day) and empty channels."""
        removed, now = 0, self.clock()
        try:
            channels = [c for c in self.builds_dir.iterdir() if c.is_dir()]
        except OSError:
            return 0
        for channel in channels:
            for entry in [*channel.glob("*.incoming"), *channel.glob("*.verifying-*")]:
                try:
                    newest = max([entry.stat().st_mtime] + [p.stat().st_mtime for p in entry.iterdir()])
                except OSError:  # removed, or claimed by a commit, meanwhile
                    continue
                if now - newest > STALE_INCOMING_S:
                    shutil.rmtree(entry, ignore_errors=True)
                    removed += 1
            try:
                channel.rmdir()  # only succeeds when empty
            except OSError:
                pass
        return removed

    # -- channels: pin, prune, delete -----------------------------------

    def _register_channel(self, channel: str) -> None:
        meta = self.channel_meta()
        if channel not in meta:
            meta[channel] = {"keep": self.keep, "pinned": []}
            jsonio.write_json(self.channels_path, meta)

    def _require_build(self, channel: str, build_id: str) -> None:
        _check(names.is_name(channel), "channel", channel)
        _check(names.is_build_id(build_id), "build id", build_id)
        if not self.build_dir(channel, build_id).is_dir():
            raise StoreError(404, f"no build {channel}/{build_id}")

    def set_pinned(self, channel: str, build_id: str, pinned: bool) -> None:
        with self.lock:
            self._require_build(channel, build_id)
            meta = self.channel_meta()
            entry = meta.setdefault(channel, {"keep": self.keep, "pinned": []})
            current = [p for p in entry.get("pinned", []) if p != build_id]
            entry["pinned"] = sorted(current + [build_id]) if pinned else current
            jsonio.write_json(self.channels_path, meta)

    def prune(self, channel: str, keep: int | None = None, protect=()) -> dict:
        """Delete all but the newest `keep` builds of a channel, never a pinned or `protect`ed one."""
        _check(names.is_name(channel), "channel", channel)
        if keep is not None:
            _check(isinstance(keep, int) and not isinstance(keep, bool) and keep >= 1, "keep", keep)
        with self.lock:
            ids = self.build_ids(channel)
            if not ids:
                raise StoreError(404, f"no builds in channel {channel}")
            if keep is not None:
                meta = self.channel_meta()
                meta.setdefault(channel, {"pinned": []})["keep"] = keep
                jsonio.write_json(self.channels_path, meta)
            limit, pinned = self.settings(channel)
            deleted = [i for i in ids[limit:] if i not in pinned and i not in protect]
            for build_id in deleted:
                self._remove(channel, build_id)
            return {"channel": channel, "keep": limit, "deleted": deleted,
                    "kept": [i for i in ids if i not in deleted]}

    def delete(self, channel: str, build_id: str) -> None:
        with self.lock:
            self._require_build(channel, build_id)
            if build_id in self.settings(channel)[1]:
                raise StoreError(409, f"{build_id} is pinned; unpin it first")
            self._remove(channel, build_id)

    def _remove(self, channel: str, build_id: str) -> None:
        """Take a build out of sight with one rename, then delete it."""
        directory = self.build_dir(channel, build_id)
        doomed = directory.with_name(build_id + ".deleting")
        shutil.rmtree(doomed, ignore_errors=True)
        os.rename(directory, doomed)
        shutil.rmtree(doomed, ignore_errors=True)
