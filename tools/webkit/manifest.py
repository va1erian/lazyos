"""``target/webkit/deps.json``: what the bundle holds, from the stamps."""

from __future__ import annotations

import json
import subprocess
import time

import buildkit
import toolenv
import wkpins


def zig_version() -> str:
    try:
        done = subprocess.run([*toolenv.zig_command(), "version"], capture_output=True, text=True)
        return done.stdout.strip()
    except (OSError, toolenv.BuildError):
        return "unknown"


def libraries() -> list[dict]:
    """One record per installed library, in build order, with sizes."""
    import recipes
    pins = {dep.name: dep for dep in [*wkpins.DEPS, *wkpins.BUNDLED]}
    out = []
    for name in recipes.ORDER:
        stamp = buildkit.STAMPS / f"{name}.json"
        if not stamp.is_file():
            continue
        record = json.loads(stamp.read_text())
        # A stamp holds what the build did; what a library is, and under which
        # licence, is the pin's (a licence fix must not wait for a rebuild).
        if name in pins:
            record.update(license=pins[name].license, why=pins[name].why)
            record.pop("notes", None)
            if pins[name].notes:
                record["notes"] = pins[name].notes
        files = record.get("files", [])
        archives = [f for f in files if f.endswith(".a")]
        record["archive_bytes"] = sum((wkpins.PREFIX / f).stat().st_size for f in archives
                                      if (wkpins.PREFIX / f).is_file())
        record["install_paths"] = files
        record.pop("files", None)
        out.append(record)
    return out


def write(timings: dict[str, float], total_seconds: float) -> None:
    """Write the manifest to the prefix and copy it to ``target/webkit``."""
    libs = libraries()
    manifest = {
        "stage": "W3", "target": "x86_64-unknown-linux-musl", "zig": zig_version(),
        "prefix": str(wkpins.PREFIX), "codegen": toolenv.CODEGEN,
        "built_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "build_seconds": total_seconds, "step_seconds": timings,
        "webkit": {"version": wkpins.jsc_pins.WEBKIT.version, "sha256": wkpins.jsc_pins.WEBKIT.sha256},
        "libraries": libs,
    }
    text = json.dumps(manifest, indent=1)
    (wkpins.PREFIX / "deps.json").write_text(text)
    wkpins.OUT.mkdir(parents=True, exist_ok=True)
    wkpins.MANIFEST.write_text(text)
