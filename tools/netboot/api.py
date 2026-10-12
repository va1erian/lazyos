"""The registry's routes (docs/netboot-plan.md section 5), independent of the HTTP server.

`Api.handle(Request) -> Response` is the whole behaviour; `registry.py` only
moves bytes between a socket and these two dataclasses, which is also what
lets the tests run most checks without one.

    GET    /healthz
    GET    /api/builds?mac=..                      the listing (its only write: dropping a failed `next`)
    GET    /files/<channel>/<id>/<name>            Content-Length, Range
    PUT    /api/builds/<channel>/<id>/<name>       into <id>.incoming
    POST   /api/builds/<channel>/<id>/commit       signed
    DELETE /api/builds/<channel>/<id>              signed
    POST   /api/channels/<channel>/pin|unpin|prune signed
    POST   /api/machines/<name>/next|add|rekey     signed
    POST   /api/machines/<name>/booting|booted     signed by the machine (HMAC) or the dev key
    GET    /boot.ipxe?mac=..   GET /builds/<channel>/<id>.ipxe     optional iPXE

A signed route takes the body described in `auth.py`. The fields that name the
route's target are mandatory in the body and must equal the path's: `channel`
and `id` for `commit` and `delete`, `channel` for `pin`, `unpin` and `prune`,
`machine` for `next`, `add`, `rekey`, `booting` and `booted`; so a captured
request cannot be pointed at another target. A file may be uploaded only once
its signed manifest is in place (`store.Store.put_file`).
"""

from __future__ import annotations

import json
import shutil
from dataclasses import dataclass, field
from pathlib import Path

import auth
import machines
import menu
import names
from store import Store, StoreError

JSON = "application/json"
TEXT = "text/plain; charset=utf-8"


@dataclass
class Request:
    method: str
    segments: list[str]          # the path split on "/" and then percent-decoded
    query: dict[str, str] = field(default_factory=dict)
    headers: object = field(default_factory=dict)   # .get(name) (http.client.HTTPMessage or dict)
    body: object = None          # file-like, positioned at the first body byte
    length: int | None = None    # Content-Length, None when absent
    host: str = ""               # the Host header, for iPXE's absolute URLs
    client: str = ""


@dataclass
class Response:
    status: int = 200
    body: bytes = b""
    content_type: str = JSON
    headers: dict[str, str] = field(default_factory=dict)
    file: Path | None = None     # a file to stream (registry.py applies Range)
    note: str = ""               # appended to the access-log line


def json_response(doc, status: int = 200, note: str = "") -> Response:
    return Response(status, (json.dumps(doc, indent=1) + "\n").encode(), JSON, note=note)


def error(status: int, message: str) -> Response:
    return json_response({"error": message}, status)


class Api:
    def __init__(self, store: Store, public_url: str | None = None):
        self.store = store
        self.machines = machines.Machines(store)
        self.public_url = public_url.rstrip("/") if public_url else None

    def handle(self, req: Request) -> Response:
        try:
            return self._route(req)
        except StoreError as e:
            return error(e.status, e.message)
        except auth.AuthError as e:
            return error(e.status, str(e))

    # -- routing --------------------------------------------------------

    def _route(self, req: Request) -> Response:
        seg, method = req.segments, req.method
        if method in ("GET", "HEAD"):
            return self._get(req, seg)
        if method == "PUT" and len(seg) == 5 and seg[:2] == ["api", "builds"]:
            return self._put_file(req, *seg[2:])
        if method == "POST":
            if len(seg) == 5 and seg[:2] == ["api", "builds"] and seg[4] == "commit":
                return self._commit(req, seg[2], seg[3])
            if len(seg) == 4 and seg[:2] == ["api", "channels"] and seg[3] in ("pin", "unpin", "prune"):
                return self._channel_action(req, seg[2], seg[3])
            if len(seg) == 4 and seg[:2] == ["api", "machines"] and seg[3] in ("next", "add", "rekey", "booting", "booted"):
                return self._machine_action(req, seg[2], seg[3])
        if method == "DELETE" and len(seg) == 4 and seg[:2] == ["api", "builds"]:
            return self._delete(req, seg[2], seg[3])
        known = self._path_known(seg)
        return error(405 if known else 404, "method not allowed" if known else "not found")

    @staticmethod
    def _path_known(seg: list[str]) -> bool:
        return (seg[:2] in (["api", "builds"], ["api", "channels"], ["api", "machines"])
                or seg in (["healthz"], []) or seg[:1] in (["files"], ["builds"]))

    def _get(self, req: Request, seg: list[str]) -> Response:
        if seg == []:
            return self._index()
        if seg == ["healthz"]:
            return self._healthz()
        if seg == ["api", "builds"]:
            return self._listing(req)
        if len(seg) == 4 and seg[0] == "files":
            path = self.store.file_path(seg[1], seg[2], seg[3])
            if path is None:
                return error(404, "no such file")
            return Response(file=path, content_type="application/octet-stream")
        if seg == ["boot.ipxe"]:
            return self._ipxe_menu(req)
        if len(seg) == 3 and seg[0] == "builds" and seg[2].endswith(".ipxe"):
            return self._ipxe_build(req, seg[1], seg[2][: -len(".ipxe")])
        return error(404, "not found")

    # -- reads ----------------------------------------------------------

    def _index(self) -> Response:
        lines = ["lazyos netboot registry"]
        for channel in self.store.channel_names():
            lines += [f"{channel}/{i}" for i in self.store.build_ids(channel)]
        return Response(body=("\n".join(lines) + "\n").encode(), content_type=TEXT)

    def _healthz(self) -> Response:
        count = sum(len(self.store.build_ids(c)) for c in self.store.channel_names())
        try:
            free = shutil.disk_usage(self.store.root).free
        except OSError:
            free = None
        return json_response({"ok": True, "builds": count, "free_bytes": free})

    def _mac(self, req: Request) -> str | None:
        raw = req.query.get("mac")
        if raw is None or raw == "":
            return None
        mac = names.normalize_mac(raw)
        if mac is None:
            raise StoreError(400, f"bad mac {raw[:40]!r}")
        return mac

    def _listing(self, req: Request) -> Response:
        mac = self._mac(req)
        doc = self.machines.listing(mac)
        who = doc["machine"]["name"] or ("unknown " + mac if mac else "anonymous")
        return json_response(doc, note=f"machine={who}")

    def _reg_url(self, req: Request) -> str:
        return self.public_url or f"http://{req.host or 'localhost'}"

    def _ipxe_menu(self, req: Request) -> Response:
        mac = self._mac(req)
        listing = self.machines.listing(mac)
        machine = listing["machine"]
        mine = next((c for c in listing["channels"] if c["name"] == machine["channel"]), None)
        builds = mine["builds"] if mine else []
        pending = (machine["next"] or {}).get("id")
        if pending and not any(b["id"] == pending for b in builds):  # queued from another channel
            other = next((b for c in listing["channels"] for b in c["builds"] if b["id"] == pending), None)
            builds = ([other] if other else []) + builds
        default = (listing["default"] or {}).get("id")
        title = f"on {mac}" if mac else "registry"
        text = menu.boot_menu(self._reg_url(req), title, builds, default, listing["timeout_ms"], pending)
        return Response(body=text.encode(), content_type=TEXT)

    def _ipxe_build(self, req: Request, channel: str, build_id: str) -> Response:
        if not (names.is_name(channel) and names.is_build_id(build_id)):
            return error(404, "not found")
        build = self.store.load_build(channel, build_id)
        if build is None:
            return error(404, "no such build")
        try:
            text = menu.build_script(self._reg_url(req), build)
        except ValueError as e:
            return error(404, str(e))
        return Response(body=text.encode(), content_type=TEXT)

    # -- writes ---------------------------------------------------------

    def _read_body(self, req: Request, limit: int) -> bytes:
        if req.length is None:
            raise StoreError(411, "Content-Length is required")
        if req.length > limit:
            raise StoreError(413, f"the body is larger than {limit} bytes")
        data = req.body.read(req.length) if req.length else b""
        if len(data) != req.length:
            raise StoreError(400, "the body ended early")
        return data

    @staticmethod
    def _bound(fields: dict, bound: dict) -> None:
        """Every `bound` field must be in the signed body and equal the path's value."""
        for key, value in bound.items():
            if key not in fields:
                raise StoreError(400, f'the signed body must carry "{key}" ({value!r}, as in the path)')
            if fields[key] != value:
                raise StoreError(400, f"the signed body names {key} {str(fields[key])[:40]!r}, "
                                      f"the path names {value!r}")

    def _signed(self, req: Request, action: str, **bound) -> dict:
        """The verified fields of a request signed with a dev key; `bound` fields are mandatory."""
        body = self._read_body(req, auth.MAX_BODY)
        header = req.headers.get(auth.SIGNATURE_HEADER)
        fields, _key = auth.verify_signed(body, header, action, self.store.known_keys(),
                                          self.store.nonces, self.store.clock())
        self._bound(fields, bound)
        return fields

    def _machine_call(self, req: Request, machine: str, action: str) -> dict:
        """The fields of a `booting`/`booted` call: HMAC under the machine's key, or the dev key."""
        body = self._read_body(req, auth.MAX_BODY)
        header = req.headers.get(auth.MACHINE_HEADER)
        if header:
            fields, who = auth.verify_machine(body, header, action, self.machines.callback_key,
                                              self.store.nonces, self.store.clock())
            if who != machine:
                raise StoreError(400, f"the header names machine {who!r}, the path names {machine!r}")
        elif req.headers.get(auth.SIGNATURE_HEADER):
            fields, _key = auth.verify_signed(body, req.headers.get(auth.SIGNATURE_HEADER), action,
                                              self.store.known_keys(), self.store.nonces, self.store.clock())
        else:
            raise auth.AuthError(f"{action} needs {auth.MACHINE_HEADER} (the machine's callback key) "
                                 f"or {auth.SIGNATURE_HEADER} (a dev key)")
        self._bound(fields, {"machine": machine})
        return fields

    def _put_file(self, req: Request, channel: str, build_id: str, name: str) -> Response:
        if req.length is None:
            raise StoreError(411, "Content-Length is required (chunked uploads are not supported)")
        size = self.store.put_file(channel, build_id, name, req.body, req.length)
        return json_response({"ok": True, "name": name, "size": size}, 201, note=f"{size} B")

    def _commit(self, req: Request, channel: str, build_id: str) -> Response:
        self._signed(req, "commit", channel=channel, id=build_id)
        result = self.store.commit(channel, build_id)
        return json_response({"ok": True, **result}, 200 if result["already"] else 201,
                             note="already present" if result["already"] else "committed")

    def _delete(self, req: Request, channel: str, build_id: str) -> Response:
        self._signed(req, "delete", channel=channel, id=build_id)
        self.store.delete(channel, build_id)
        return json_response({"ok": True, "deleted": build_id})

    def _channel_action(self, req: Request, channel: str, action: str) -> Response:
        if not names.is_name(channel):
            raise StoreError(400, f"bad channel: {channel[:80]!r}")
        fields = self._signed(req, action, channel=channel)
        if action == "prune":
            result = self.store.prune(channel, fields.get("keep"), self.machines.pending_ids())
            return json_response({"ok": True, **result}, note=f"deleted {len(result['deleted'])}")
        build_id = fields.get("id")
        if not names.is_build_id(build_id):
            raise StoreError(400, f"bad build id: {str(build_id)[:80]!r}")
        self.store.set_pinned(channel, build_id, action == "pin")
        return json_response({"ok": True, "channel": channel, "id": build_id, "pinned": action == "pin"})

    def _machine_action(self, req: Request, name: str, action: str) -> Response:
        if not names.is_name(name):
            raise StoreError(400, f"bad machine name: {name[:80]!r}")
        if action in ("booting", "booted"):
            fields = self._machine_call(req, name, action)
            method = self.machines.booting if action == "booting" else self.machines.booted
            return json_response({"ok": True, **method(name, fields.get("id"))},
                                 note=f"machine={name} id={fields.get('id')}")
        fields = self._signed(req, action, machine=name)
        if action == "next":
            result = self.machines.set_next(name, fields.get("id"))
        elif action == "add":
            result = self.machines.add(name, fields.get("mac"), fields.get("channel"), fields.get("timeout_ms"))
            return json_response({"ok": True, **result}, 201, note=f"machine={name} added")
        else:
            result = self.machines.rekey(name)
        return json_response({"ok": True, **result}, note=f"machine={name} {action}")
