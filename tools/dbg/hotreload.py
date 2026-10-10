"""Hot reload of a LazyOS service through `dbgd`'s control tier
(docs/dbgd-plan.md, v2; issue #701).

    with DbgClient(host, port, key) as dbg:
        verdict = reload(dbg, "usbd", image_binary(IMAGE, "usbd"),
                         reconnect=lambda: DbgClient(host, port, key))

The binary is uploaded in chunks, `init` on the box checks its SHA-256,
restarts the service from it and holds it to a trial: an exit or a failed
spawn before the trial ends rolls back to the image's binary, a run still
alive at the deadline is committed. A reload lasts until `revert` or a
reboot (the box's stick is never written).

An app is swapped with its package instead (`install_app`): `pkgd`
installs the `.lzp` (a core app too) and `init` relaunches the running
instances in their sessions.

The box must be built with `LAZYOS_DBGD=1 LAZYOS_DBGD_CONTROL=1`.
"""

from __future__ import annotations

import base64
import hashlib
import io
import re
import subprocess
import time
import zipfile
from pathlib import Path
from typing import Callable

from dbgclient import DbgError

ROOT = Path(__file__).resolve().parents[2]
#: Bytes per `service.upload` (`dbgwire::methods::UPLOAD_CHUNK`).
CHUNK = 6 * 1024
#: `dbgwire::control::CONFIRM`.
CONFIRM = "control"
DEFAULT_TRIAL_MS = 10_000


def image_binary(image: Path, name: str, program: str | None = None) -> bytes:
    """`/system/bin/<program or name>` from a built image's OS volume, read
    with `libs/ext2fs`'s `osread` (the bytes the image build put there, so
    built with the same switches as the box)."""
    path = f"/system/bin/{program or name}"
    result = subprocess.run(
        ["cargo", "run", "-q", "-p", "ext2fs", "--example", "osread", "--",
         str(image), "cat", path],
        cwd=ROOT, capture_output=True, check=False)
    if result.returncode != 0 or not result.stdout:
        raise RuntimeError(f"cannot read {path} from {image}: "
                           f"{result.stderr.decode(errors='replace').strip()}")
    return result.stdout


def begin(dbg) -> None:
    """Open the control tier for this connection."""
    dbg.call("control.begin", confirm=CONFIRM)


def upload(dbg, name: str | None, data: bytes,
           progress: Callable[[int, int], None] | None = None) -> str:
    """Stage `data` as the new binary of service `name` (`None`: as the
    package `install_app` installs); returns its SHA-256 (hex)."""
    total = len(data)
    for offset in range(0, total, CHUNK):
        chunk = data[offset:offset + CHUNK]
        encoded = base64.b64encode(chunk).decode()
        if name is None:
            dbg.call("app.upload", offset=offset, total=total, data=encoded)
        else:
            dbg.call("service.upload", name=name, offset=offset, total=total, data=encoded)
        if progress:
            progress(min(offset + CHUNK, total), total)
    return hashlib.sha256(data).hexdigest()


def state_of(dbg, name: str) -> dict | None:
    """`name`'s row of `service.reloads`, if it was ever reloaded."""
    rows = dbg.call("service.reloads")["reloads"]
    return next((r for r in rows if r["name"] == name), None)


def wait_verdict(dbg, name: str, sha256: str, timeout: float,
                 reconnect: Callable[[], object] | None = None) -> dict:
    """Poll until `init` committed or rolled back the reload of `name` with
    `sha256`. A reload of the service carrying the connection (`netdrv`,
    `netd`) drops it: `reconnect` returns a fresh, authenticated client."""
    end = time.monotonic() + timeout
    last: dict = {"name": name, "state": "timeout", "detail": "no verdict in time"}
    while time.monotonic() < end:
        try:
            row = state_of(dbg, name)
        except (OSError, DbgError) as error:
            if reconnect is None or (isinstance(error, DbgError) and error.code != -1):
                raise
            last = {"name": name, "state": "unreachable", "detail": str(error)}
            dbg.close()
            time.sleep(1.0)
            try:
                dbg = reconnect()
            except (OSError, DbgError):
                pass
            continue
        if row is not None:
            last = row
            if row["state"] == "committed" and row["sha256"] == sha256:
                return row
            if row["state"] in ("rolled-back", "reverted"):
                return row
        time.sleep(0.5)
    return last


def reload(dbg, name: str, data: bytes, trial_ms: int = DEFAULT_TRIAL_MS,
           reconnect: Callable[[], object] | None = None,
           progress: Callable[[int, int], None] | None = None) -> dict:
    """Upload `data`, reload `name` from it and wait for `init`'s verdict:
    the final `service.reloads` row (`state` `committed` or `rolled-back`)."""
    begin(dbg)
    sha = upload(dbg, name, data, progress)
    try:
        dbg.call("service.reload", name=name, sha256=sha, trial_ms=trial_ms)
    except (OSError, DbgError) as error:
        # Reloading the NIC driver can take the answer with it; the verdict
        # is read after reconnecting.
        if reconnect is None or (isinstance(error, DbgError) and error.code != -1):
            raise
    verdict = wait_verdict(dbg, name, sha, trial_ms / 1000 + 30, reconnect)
    return {**verdict, "uploaded_sha256": sha, "bytes": len(data)}


def with_version(package: bytes, version: str) -> bytes:
    """`package` with its manifest's `version` replaced, everything else
    copied as it was: `pkgd` refuses an install at the version already on the
    box, so a rebuilt app needs a new one to be swapped in."""
    source = zipfile.ZipFile(io.BytesIO(package))
    out = io.BytesIO()
    changed = 0
    with zipfile.ZipFile(out, "w") as target:
        for info in source.infolist():
            data = source.read(info)
            if info.filename == "manifest.toml":
                data, changed = re.subn(rb'(?m)^version\s*=\s*"[^"]*"',
                                        f'version = "{version}"'.encode(), data, count=1)
            target.writestr(info, data, compress_type=info.compress_type)
    if changed != 1:
        raise ValueError("the package's manifest.toml has no `version = \"...\"` line")
    return out.getvalue()


def install_app(dbg, package: bytes, relaunch: bool = True,
                progress: Callable[[int, int], None] | None = None) -> dict:
    """Upload an `.lzp`, have `pkgd` install it and (by default) `init`
    relaunch its running instances. Returns `app.install`'s answer:
    `system_name`, `version`, `stopped`, `started`."""
    begin(dbg)
    sha = upload(dbg, None, package, progress)
    return dbg.call("app.install", sha256=sha, relaunch=relaunch)
