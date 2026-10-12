"""Fixtures shared by the netboot tests (not a test module itself).

`make_build` writes what the Rust build side leaves in `target/netboot/` (four
small files, a manifest with real digests, a `manifest.sig` over its exact
bytes); `Harness` runs a real registry on a loopback port over a temporary
root with a key registered, and speaks HTTP to it.
"""

from __future__ import annotations

import hashlib
import http.client
import json
import tempfile
import threading
import time
from pathlib import Path

import auth
import ed25519
import keys
import registry

REV = "2eeee9361" + "0" * 31


def build_id(minute: int = 0, rev8: str = "2eeee936", dirty: bool = False) -> str:
    return f"20261012-09{minute:02d}00-{rev8}" + ("-dirty" if dirty else "")


def pattern(n: int, seed: int) -> bytes:
    """`n` deterministic bytes that do not repeat in short cycles."""
    return hashlib.shake_128(bytes([seed])).digest(n)


def make_build(directory: Path, seed: bytes, bid: str | None = None, ramdisk_bytes: int = 200_000,
               payload_seed: int = 1, sign: bool = True, extra: dict | None = None) -> dict:
    """A fake `target/netboot/`; returns `{"id", "files": {name: bytes}, "manifest": bytes}`."""
    bid = bid or build_id()
    contents = {
        "kernel-x86_64": pattern(70_000, payload_seed),
        "ramdisk": pattern(ramdisk_bytes, payload_seed + 100),
        "bootx64.efi": b"MZ" + pattern(600, payload_seed + 200),
        "boot.json": b'{"log_level": "Trace"}\n',
    }
    manifest = {
        "id": bid, "revision": REV, "dirty": bid.endswith("-dirty"),
        "built_at": "2026-10-12T09:30:00Z", "host": "devbox",
        "switches": ["LAZYOS_DESKTOP", "LAZYOS_NETD"],
        "files": {n: {"size": len(d), "sha256": hashlib.sha256(d).hexdigest()}
                  for n, d in contents.items()},
    }
    manifest.update(extra or {})
    raw = (json.dumps(manifest, indent=2) + "\n").encode()
    directory.mkdir(parents=True, exist_ok=True)
    for name, data in contents.items():
        (directory / name).write_bytes(data)
    (directory / "manifest.json").write_bytes(raw)
    if sign:
        (directory / "manifest.sig").write_text(keys.format_signature(ed25519.sign(seed, raw)))
    return {"id": bid, "files": contents, "manifest": raw}


class Harness:
    """A running registry on `127.0.0.1:<free port>` with one registered key."""

    def __init__(self, keep: int = 10, register_key: bool = True, machines: dict | None = None,
                 clock=time.time, root: Path | None = None, public_url: str | None = None):
        self._tmp = tempfile.TemporaryDirectory(ignore_cleanup_errors=True)
        self.tmp = Path(self._tmp.name)
        self.root = root or self.tmp / "registry"
        self.seed = ed25519.generate_seed()
        self.pub = ed25519.public_key(self.seed)
        self.keep, self.clock, self.public_url = keep, clock, public_url
        self.log: list[str] = []
        self.root.mkdir(parents=True, exist_ok=True)
        if register_key:
            (self.root / "keys.json").write_text(json.dumps({"keys": [keys.keys_entry(self.pub, "test")]}))
        if machines is not None:
            (self.root / "machines.json").write_text(json.dumps(machines))
        self.start()

    def start(self) -> None:
        self.server = registry.serve(self.root, "127.0.0.1:0", None, self.keep, self.log, self.public_url, self.clock)
        self.port = self.server.server_address[1]
        self.thread = threading.Thread(target=lambda: self.server.serve_forever(poll_interval=0.02), daemon=True)
        self.thread.start()

    def stop(self) -> None:
        self.server.shutdown()
        self.server.server_close()

    def close(self) -> None:
        self.stop()
        self._tmp.cleanup()

    @property
    def url(self) -> str:
        return f"http://127.0.0.1:{self.port}"

    def request(self, method: str, path: str, body: bytes | None = None, headers: dict | None = None):
        """`(status, headers, body bytes)`."""
        conn = http.client.HTTPConnection("127.0.0.1", self.port, timeout=20)
        try:
            conn.request(method, path, body=body, headers=headers or {})
            response = conn.getresponse()
            return response.status, response, response.read()
        finally:
            conn.close()

    def json(self, method: str, path: str, body: bytes | None = None, headers: dict | None = None):
        status, _, data = self.request(method, path, body, headers)
        return status, json.loads(data) if data else None

    def signed(self, method: str, path: str, action: str, fields: dict | None = None,
               seed: bytes | None = None, now: float | None = None, nonce: str | None = None):
        body, header = auth.sign_body(seed or self.seed, action, fields, now, nonce)
        return self.json(method, path, body, {auth.SIGNATURE_HEADER: header,
                                              "Content-Type": "application/json"})

    def upload(self, directory: Path, build: dict, channel: str = "dev", commit: bool = True,
               skip: tuple = (), seed: bytes | None = None):
        """PUT every file of a `make_build` directory, then commit; returns the commit's `(status, doc)`."""
        names_ = list(build["files"]) + ["manifest.json", "manifest.sig"]
        for name in names_:
            path = directory / name
            if name in skip or not path.exists():
                continue
            status, doc = self.json("PUT", f"/api/builds/{channel}/{build['id']}/{name}", path.read_bytes())
            if status != 201:
                return status, doc
        if not commit:
            return 201, None
        return self.signed("POST", f"/api/builds/{channel}/{build['id']}/commit", "commit",
                           {"channel": channel, "id": build["id"]}, seed)

    def push(self, build_dirname: str, bid: str, channel: str = "dev", **kw):
        """Make a build in the harness's temp dir and upload it."""
        directory = self.tmp / build_dirname
        build = make_build(directory, self.seed, bid, **kw)
        return build, directory, self.upload(directory, build, channel)


def put_dir(store, channel: str, bid: str, directory: Path, names: list[str]) -> None:
    """Store a `make_build` directory's files straight into the store's incoming directory."""
    for name in names:
        with open(directory / name, "rb") as f:
            store.put_file(channel, bid, name, f, (directory / name).stat().st_size)
