#!/usr/bin/env python3
"""Upload screenshot files to a public image host and print ``name<TAB>url``.

Used by CI to embed screenshots inline in pull-request comments (GitHub renders
external image URLs). Standard library only.

Hosts are tried in order until one succeeds, because some hosts rate-limit or
block datacenter IPs (e.g. Catbox returns HTTP 412 from CI runners).

    1. Imgur            - if IMGUR_CLIENT_ID is set (most reliable)
    2. Catbox           - anonymous, no account
    3. Litterbox        - Catbox's temporary sibling (72h)
    4. Uguu             - anonymous
    5. 0x0.st           - anonymous, fair-use
    6. tmpfiles.org     - anonymous, 1h

Usage
-----
    python tools/screenshot/upload_image.py shots/shot_2s.png shots/shot_5s.png
"""

from __future__ import annotations

import json
import os
import sys
import urllib.request
import uuid
from pathlib import Path

CATBOX_URL = "https://catbox.moe/user/api.php"
LITTERBOX_URL = "https://litterbox.catbox.moe/resources/internals/api.php"
UGUU_URL = "https://uguu.se/upload.php"
ZEROX0_URL = "https://0x0.st"
TMPFILES_URL = "https://tmpfiles.org/api/v1/upload"
IMGUR_URL = "https://api.imgur.com/3/image"


def _multipart(fields: dict[str, str], files: dict[str, tuple[str, bytes, str]]):
    boundary = "----lazyos" + uuid.uuid4().hex
    body = bytearray()
    for name, value in fields.items():
        body += f"--{boundary}\r\n".encode()
        body += f'Content-Disposition: form-data; name="{name}"\r\n\r\n'.encode()
        body += value.encode() + b"\r\n"
    for name, (filename, data, content_type) in files.items():
        body += f"--{boundary}\r\n".encode()
        body += (
            f'Content-Disposition: form-data; name="{name}"; filename="{filename}"\r\n'
        ).encode()
        body += f"Content-Type: {content_type}\r\n\r\n".encode()
        body += data + b"\r\n"
    body += f"--{boundary}--\r\n".encode()
    return bytes(body), f"multipart/form-data; boundary={boundary}"


def _post(url: str, data: bytes, content_type: str, headers: dict[str, str]) -> bytes:
    request = urllib.request.Request(url, data=data, method="POST")
    request.add_header("Content-Type", content_type)
    request.add_header("User-Agent", "lazyos-screenshot-upload/1.0 (+https://github.com/va1erian/lazyos)")
    request.add_header("Accept", "*/*")
    for key, value in headers.items():
        request.add_header(key, value)
    with urllib.request.urlopen(request, timeout=120) as response:
        return response.read()


def _png_fields(field: str, path: Path) -> dict[str, tuple[str, bytes, str]]:
    return {field: (path.name, path.read_bytes(), "image/png")}


def upload_imgur(path: Path, client_id: str) -> str:
    body, content_type = _multipart({}, _png_fields("image", path))
    result = _post(IMGUR_URL, body, content_type, {"Authorization": f"Client-ID {client_id}"})
    payload = json.loads(result)
    if not payload.get("success"):
        raise RuntimeError(f"imgur: {payload}")
    return payload["data"]["link"]


def upload_catbox(path: Path) -> str:
    body, content_type = _multipart({"reqtype": "fileupload"}, _png_fields("fileToUpload", path))
    result = _post(CATBOX_URL, body, content_type, {}).decode("utf-8").strip()
    if not result.startswith("http"):
        raise RuntimeError(f"catbox: {result!r}")
    return result


def upload_litterbox(path: Path) -> str:
    fields = {"reqtype": "fileupload", "time": "72h"}
    body, content_type = _multipart(fields, _png_fields("fileToUpload", path))
    result = _post(LITTERBOX_URL, body, content_type, {}).decode("utf-8").strip()
    if not result.startswith("http"):
        raise RuntimeError(f"litterbox: {result!r}")
    return result


def upload_uguu(path: Path) -> str:
    body, content_type = _multipart({}, _png_fields("files[]", path))
    result = json.loads(_post(UGUU_URL, body, content_type, {}))
    if not result.get("success"):
        raise RuntimeError(f"uguu: {result}")
    return result["files"][0]["url"]


def upload_0x0(path: Path) -> str:
    body, content_type = _multipart({}, _png_fields("file", path))
    result = _post(ZEROX0_URL, body, content_type, {}).decode("utf-8").strip()
    if not result.startswith("http"):
        raise RuntimeError(f"0x0: {result!r}")
    return result


def upload_tmpfiles(path: Path) -> str:
    body, content_type = _multipart({}, _png_fields("file", path))
    result = json.loads(_post(TMPFILES_URL, body, content_type, {}))
    url = result["data"]["url"].replace("tmpfiles.org/", "tmpfiles.org/dl/")
    return url


def hosts() -> list[tuple[str, callable]]:
    chain: list[tuple[str, callable]] = []
    client_id = os.environ.get("IMGUR_CLIENT_ID", "").strip()
    if client_id:
        chain.append(("imgur", lambda p: upload_imgur(p, client_id)))
    chain += [
        ("catbox", upload_catbox),
        ("litterbox", upload_litterbox),
        ("uguu", upload_uguu),
        ("0x0", upload_0x0),
        ("tmpfiles", upload_tmpfiles),
    ]
    return chain


def main(argv: list[str]) -> int:
    if not argv:
        print(__doc__)
        return 2

    chain = hosts()
    failures = 0
    for name in argv:
        path = Path(name)
        url = None
        errors: list[str] = []
        for host_name, upload in chain:
            try:
                url = upload(path)
                print(f"# {path.name}: uploaded via {host_name}", file=sys.stderr)
                break
            except Exception as exc:  # noqa: BLE001 - try the next host
                errors.append(f"{host_name}={exc}")
        if url:
            print(f"{path.name}\t{url}")
        else:
            print(f"{path.name}\tERROR: {', '.join(errors)}", file=sys.stderr)
            failures += 1
    return 1 if failures == len(argv) else 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
