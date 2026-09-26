#!/usr/bin/env python3
"""Upload screenshot files to a public image host and print ``name<TAB>url``.

Used by CI to embed screenshots inline in pull-request comments (GitHub renders
external image URLs). Standard library only.

Backend selection
-----------------
* If the ``IMGUR_CLIENT_ID`` environment variable is set, upload to Imgur.
* Otherwise upload anonymously to Catbox (no account required).

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
    request.add_header("User-Agent", "lazyos-screenshot-upload/1.0")
    for key, value in headers.items():
        request.add_header(key, value)
    with urllib.request.urlopen(request, timeout=120) as response:
        return response.read()


def upload_catbox(path: Path) -> str:
    body, content_type = _multipart(
        {"reqtype": "fileupload"},
        {"fileToUpload": (path.name, path.read_bytes(), "image/png")},
    )
    result = _post(CATBOX_URL, body, content_type, {}).decode("utf-8").strip()
    if not result.startswith("http"):
        raise RuntimeError(f"catbox upload failed: {result!r}")
    return result


def upload_imgur(path: Path, client_id: str) -> str:
    body, content_type = _multipart(
        {},
        {"image": (path.name, path.read_bytes(), "image/png")},
    )
    result = _post(IMGUR_URL, body, content_type, {"Authorization": f"Client-ID {client_id}"})
    payload = json.loads(result)
    if not payload.get("success"):
        raise RuntimeError(f"imgur upload failed: {payload}")
    return payload["data"]["link"]


def main(argv: list[str]) -> int:
    if not argv:
        print(__doc__)
        return 2

    client_id = os.environ.get("IMGUR_CLIENT_ID", "").strip()
    failures = 0
    for name in argv:
        path = Path(name)
        try:
            if client_id:
                url = upload_imgur(path, client_id)
            else:
                url = upload_catbox(path)
            print(f"{path.name}\t{url}")
        except Exception as exc:  # noqa: BLE001 - report and continue
            print(f"{path.name}\tERROR: {exc}", file=sys.stderr)
            failures += 1
    return 1 if failures == len(argv) else 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
