"""A small HTTP client for the registry's API (used by `push.py`; standard library only)."""

from __future__ import annotations

import http.client
import json
import urllib.parse
from pathlib import Path

import auth

CHUNK = 1 << 20


class RegistryError(Exception):
    """The registry (or the network) said no; `status` is the HTTP code, 0 for a network failure."""

    def __init__(self, status: int, message: str):
        super().__init__(message)
        self.status, self.message = status, message


class Client:
    def __init__(self, url: str, seed: bytes | None = None, timeout: float = 120.0):
        parts = urllib.parse.urlsplit(url if "//" in url else "http://" + url)
        if parts.scheme not in ("http", "https") or not parts.hostname:
            raise ValueError(f"bad registry URL {url!r} (want http://HOST:PORT)")
        self.parts, self.seed, self.timeout = parts, seed, timeout
        self.base = parts.path.rstrip("/")

    @property
    def url(self) -> str:
        return f"{self.parts.scheme}://{self.parts.netloc}{self.base}"

    def _conn(self) -> http.client.HTTPConnection:
        cls = http.client.HTTPSConnection if self.parts.scheme == "https" else http.client.HTTPConnection
        return cls(self.parts.hostname, self.parts.port, timeout=self.timeout)

    @staticmethod
    def _decode(status: int, data: bytes):
        try:
            doc = json.loads(data) if data else {}
        except ValueError:
            doc = {"error": data[:200].decode("utf-8", "replace")}
        if status >= 400:
            raise RegistryError(status, doc.get("error", f"HTTP {status}") if isinstance(doc, dict) else str(doc))
        return doc

    def request(self, method: str, path: str, body: bytes | None = None, headers: dict | None = None):
        conn = self._conn()
        try:
            conn.request(method, self.base + path, body=body, headers=headers or {})
            response = conn.getresponse()
            return self._decode(response.status, response.read())
        except OSError as e:
            raise RegistryError(0, f"cannot reach {self.url}: {e}") from e
        finally:
            conn.close()

    def signed(self, method: str, path: str, action: str, fields: dict | None = None):
        if self.seed is None:
            raise RegistryError(0, "this request needs the signing key (key.sec)")
        body, header = auth.sign_body(self.seed, action, fields)
        return self.request(method, path, body, {auth.SIGNATURE_HEADER: header,
                                                 "Content-Type": "application/json"})

    def listing(self, mac: str | None = None) -> dict:
        return self.request("GET", "/api/builds" + (f"?mac={urllib.parse.quote(mac)}" if mac else ""))

    def put_file(self, channel: str, build_id: str, name: str, path: Path, progress=None) -> dict:
        """Stream a file to the build's incoming directory (never loaded whole into memory)."""
        size = path.stat().st_size
        conn = self._conn()
        try:
            conn.putrequest("PUT", f"{self.base}/api/builds/{channel}/{build_id}/{name}")
            conn.putheader("Content-Length", str(size))
            conn.putheader("Content-Type", "application/octet-stream")
            conn.endheaders()
            sent = 0
            try:
                with open(path, "rb") as f:
                    while chunk := f.read(CHUNK):
                        conn.send(chunk)
                        sent += len(chunk)
                        if progress:
                            progress(sent, size)
            except OSError:
                # the registry may have refused before reading the body: its answer says why
                pass
            response = conn.getresponse()
            return self._decode(response.status, response.read())
        except OSError as e:
            raise RegistryError(0, f"upload of {name} failed: {e}") from e
        finally:
            conn.close()

    def add_machine(self, name: str, mac: str, channel: str | None = None,
                    timeout_ms: int | None = None) -> dict:
        fields = {"machine": name, "mac": mac}
        if channel:
            fields["channel"] = channel
        if timeout_ms is not None:
            fields["timeout_ms"] = timeout_ms
        return self.signed("POST", f"/api/machines/{name}/add", "add", fields)

    def rekey_machine(self, name: str) -> dict:
        return self.signed("POST", f"/api/machines/{name}/rekey", "rekey", {"machine": name})

    def callback(self, machine: str, action: str, build_id: str, machine_key: str | None = None) -> dict:
        """Post `booting` or `booted`: signed with the machine's callback key (HMAC) if given, else the dev key."""
        path = f"/api/machines/{machine}/{action}"
        if machine_key:
            body, header = auth.sign_machine(machine_key, machine, action, {"id": build_id})
            return self.request("POST", path, body, {auth.MACHINE_HEADER: header,
                                                     "Content-Type": "application/json"})
        return self.signed("POST", path, action, {"machine": machine, "id": build_id})

    def commit(self, channel: str, build_id: str) -> dict:
        return self.signed("POST", f"/api/builds/{channel}/{build_id}/commit", "commit",
                           {"channel": channel, "id": build_id})

    def set_next(self, machine: str, build_id: str) -> dict:
        return self.signed("POST", f"/api/machines/{machine}/next", "next",
                           {"machine": machine, "id": build_id})

    def pin(self, channel: str, build_id: str, pinned: bool = True) -> dict:
        action = "pin" if pinned else "unpin"
        return self.signed("POST", f"/api/channels/{channel}/{action}", action,
                           {"channel": channel, "id": build_id})

    def prune(self, channel: str, keep: int | None = None) -> dict:
        fields = {"channel": channel}
        if keep is not None:
            fields["keep"] = keep
        return self.signed("POST", f"/api/channels/{channel}/prune", "prune", fields)

    def delete(self, channel: str, build_id: str) -> dict:
        return self.signed("DELETE", f"/api/builds/{channel}/{build_id}", "delete",
                           {"channel": channel, "id": build_id})
