#!/usr/bin/env python3
"""Build the xui app for LazyOS (issue #114).

The app is an ordinary Rust program built for ``x86_64-unknown-linux-musl``
(static): it is *not* part of the OS workspace. It is embedded in the disk
image by the root ``build.rs`` when ``LAZYOS_XUI_APP`` points at one of the
outputs, and booted by the kernel with ``LAZYOS_XUID=1``.

On Windows the musl target has no host linker, so the script points cargo at
the toolchain's bundled ``rust-lld`` with self-contained linking (the same
recipe the Linux-ABI fixtures need); on other hosts the default toolchain
linker is used. The environment is set only for the cargo subprocess, so a
later LazyOS workspace build is unaffected.

Usage::

    python tools/xui/build.py
    python tools/xui/build.py --debug

Output: target/xui/xui-m0.elf, target/xui/xui-counter.elf,
target/xui/xui-sysmon.elf, target/xui/xui-fabricmon.elf,
target/xui/xui-client.elf and target/xui/xui-term.elf, plus xui-editor.elf,
xui-paint.elf, xui-files.elf (the migrated document apps), xui-writer.elf
(LazyWriter, cargo bin ``writer``) and xui-archiver.elf (the Archiver), and a
JSON map
on stdout. If the musl target or toolchain is unavailable the script reports
what it could build and exits 0, so a CI job can skip the visual run.

The Docs app (``xui-docs.elf``, Markdown rendered by litehtml) is built last, in
its own cargo invocation and target directory, with the zig toolchain
(``tools/xui/zig.py``): litehtml is C++, and only that package pulls it in.
Without zig it is skipped with a warning and every other app still builds.
LazyWeb (``xui-lazyweb.elf``, the web browser on the NetSurf core, C and
GPL-2.0-only) is built the same way, in its own cargo invocation sharing that
target directory. Mail (``xui-mail.elf``, esMail's IMAP/SMTP core with SQLite
and litehtml) is built the same way, only with ``--mail`` or ``LAZYOS_MAIL=1``.

``xui-core``, ``xui-canvas`` and ``xui-icons`` are git dependencies on
``va1erian/xui`` at a single pinned revision; ``xui-canvas`` is built with
``default-features = false`` so its software painter core (in-memory font
registration, natural-width alignment, borrowed pixels, ``OffscreenBackend``) is
used without ``winit``/``softbuffer``/``glutin``/``glow``/``arboard``/``xui-gpu``.
No vendored copy or ``[patch]`` is involved.
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import zig  # noqa: E402

ROOT = Path(__file__).resolve().parent.parent.parent
APP = ROOT / "xui-app"
TARGET = "x86_64-unknown-linux-musl"
OUT_DIR = ROOT / "target" / "xui"
# The Docs app has its own cargo target directory: its zig-linked build uses a
# different RUSTFLAGS environment, which would otherwise invalidate (and
# alternately rebuild) every dependency the other apps share.
DOCS_TARGET_DIR = ROOT / "target" / "xui-zig"
DOCS_PACKAGE = "xui-docs"
# LazyWeb, the web browser (`xui-app/web`): NetSurf is C, so it is built with
# zig like Docs, in the same target directory (same environment).
WEB_PACKAGE = "lazyweb"
WEB_ELF = "xui-lazyweb.elf"
# Mail (esMail, docs/mail.md) links SQLite and litehtml, so it is built like
# the Docs app; only on request (`--mail`, or `LAZYOS_MAIL=1` in the
# environment), since its mail core is a long build no other image needs.
MAIL_PACKAGE = "xui-mail"
BINS = {
    "xui-m0": "xui-m0.elf",
    "xui-counter": "xui-counter.elf",
    "xui-sysmon": "xui-sysmon.elf",
    "xui-fabricmon": "xui-fabricmon.elf",
    "xui-widget": "xui-widget.elf",
    "xui-client": "xui-client.elf",
    "xui-term": "xui-term.elf",
    # The migrated portable apps (issues #162/#159); every desktop image ships
    # them (build.rs `DESKTOP_XUI_APPS`).
    "xui-editor": "xui-editor.elf",
    "xui-paint": "xui-paint.elf",
    "xui-files": "xui-files.elf",
    # LazyWriter, the word processor (issue #533): its cargo bin is `writer`,
    # built under the same `xui-<stem>.elf` name as the other apps.
    "writer": "xui-writer.elf",
    # The Archiver, a 7-Zip-style archive manager (docs/archiver-plan.md).
    "xui-archiver": "xui-archiver.elf",
    # The Settings app (confd-backed configuration panel).
    "xui-settings": "xui-settings.elf",
    # The Config app (generic confd registry editor).
    "xui-confd": "xui-confd.elf",
    # The network apps (status/configuration, and the Net Tools demo); a
    # desktop image ships them with the network stack (`LAZYOS_NETD=1`).
    "xui-network": "xui-network.elf",
    "xui-nettools": "xui-nettools.elf",
    # The print spooler service (no window, docs/printing-plan.md P6); a
    # desktop image with the network stack ships it at /system/bin/printd.
    "xui-printd": "xui-printd.elf",
    # The Installer app (`.lzp` package consent and removal, `docs/packages.md`).
    # `build_support/xui_embed.rs` places it at /system/bin/installer.
    "xui-installer": "xui-installer.elf",
    # LazyShell, the desktop shell (issue #157): `build.rs` embeds it as
    # /system/bin/lazyshell on the desktop profile unless LAZYOS_SHELL=0.
    "xui-shell": "xui-shell.elf",
    # The Devices app (issue #481): owners, rights and the driver class rules.
    # `build_support/xui_embed.rs` places it at /system/bin/devices.
    "xui-devices": "xui-devices.elf",
    # Calculator: A basic calculator.
    "xui-calc": "xui-calc.elf",
    # PDF Viewer: Read PDF documents.
    "xui-pdf": "xui-pdf.elf",
    # Tray Demo: Shows a taskbar tray icon and reacts to it.
    "xui-traydemo": "xui-traydemo.elf",
    # Volume: Sound volume in the taskbar tray.
    "xui-volume": "xui-volume.elf",
    # Network Status: Network status in the taskbar tray.
    "xui-netstatus": "xui-netstatus.elf",
}


def run(
    cmd: list[str], env: dict[str, str] | None = None, stream: bool = False
) -> subprocess.CompletedProcess:
    """Run a tool; stdout is always captured (this script's own stdout is the
    JSON result). With `stream` its stderr goes straight to ours, so a long
    cargo build shows its progress (and timing) in a CI log instead of nothing
    until it ends; `stderr` is then `None` on the result."""
    return subprocess.run(
        cmd, cwd=ROOT, stdout=subprocess.PIPE, stderr=None if stream else subprocess.PIPE,
        text=True, env=env,
    )


def ensure_target() -> bool:
    """Return True if the musl target is installed (adding it if needed)."""
    installed = run(["rustup", "target", "list", "--installed"])
    if TARGET in installed.stdout:
        return True
    added = run(["rustup", "target", "add", TARGET])
    if added.returncode != 0:
        print(f"warning: cannot add {TARGET}: {added.stderr.strip()}", file=sys.stderr)
        return False
    return True


def build_env() -> dict[str, str]:
    """The cargo environment, with the bundled lld on hosts without a musl cc."""
    env = dict(os.environ)
    if os.name != "nt":
        return env
    sysroot = run(["rustc", "--print", "sysroot"]).stdout.strip()
    version = run(["rustc", "-vV"]).stdout
    host = next(
        (line.split(":", 1)[1].strip() for line in version.splitlines() if line.startswith("host:")),
        "",
    )
    linker = Path(sysroot) / "lib" / "rustlib" / host / "bin" / "rust-lld.exe"
    if linker.is_file():
        # Target-specific variables so the host build scripts and proc macros
        # keep the default linker; a bare RUSTFLAGS would leak into them.
        prefix = f"CARGO_TARGET_{TARGET.upper().replace('-', '_')}"
        env.setdefault(f"{prefix}_LINKER", str(linker))
        env.setdefault(f"{prefix}_RUSTFLAGS", "-C linker-flavor=ld.lld")
    return env


def zig_env() -> dict[str, str] | None:
    """The cargo environment that compiles C/C++ and links with zig, or None
    (with a warning) when zig is not installed."""
    command = zig.find_zig()
    if command is None:
        print(f"warning: zig not found (install: {zig.INSTALL_HINT})", file=sys.stderr)
        return None
    found = zig.version(command)
    if found != zig.ZIG_VERSION:
        print(f"warning: zig {found} found, {zig.ZIG_VERSION} is the tested version", file=sys.stderr)
    wrappers = zig.write_wrappers(command, OUT_DIR / "zig")
    env = dict(os.environ)
    env.update(zig.cargo_env(TARGET, wrappers))
    return env


def build_zig_package(package: str, env: dict[str, str], debug: bool) -> str | None:
    """Build one zig-linked package's binary (named like the package); return
    its ELF path. A compile error is fatal so CI cannot silently ship an image
    without the app."""
    cargo = [
        "cargo",
        "build",
        "--manifest-path",
        str(APP / "Cargo.toml"),
        "-p",
        package,
        "--bin",
        package,
        "--target",
        TARGET,
        "--target-dir",
        str(DOCS_TARGET_DIR),
    ]
    if not debug:
        cargo.append("--release")
    build = run(cargo, env=env, stream=True)
    if build.returncode != 0:
        print(f"error: {package} build failed", file=sys.stderr)
        raise SystemExit(1)
    source = DOCS_TARGET_DIR / TARGET / ("debug" if debug else "release") / package
    return str(source) if source.is_file() else None


def build_zig_apps(debug: bool, lazyweb: bool = True, mail: bool = False) -> dict[str, str]:
    """Build the zig-linked apps (Docs, LazyWeb unless `lazyweb` is False,
    and Mail when `mail`); return `{package: elf}` of those built. A missing
    zig is a skip (warning), like a missing musl target."""
    env = zig_env()
    if env is None:
        print(f"warning: skipping {DOCS_PACKAGE} and {WEB_PACKAGE}", file=sys.stderr)
        return {}
    built: dict[str, str] = {}
    apps = [(DOCS_PACKAGE, f"{DOCS_PACKAGE}.elf")]
    if lazyweb:
        apps.append((WEB_PACKAGE, WEB_ELF))
    if mail:
        apps.append((MAIL_PACKAGE, f"{MAIL_PACKAGE}.elf"))
    for package, disk_name in apps:
        source = build_zig_package(package, env, debug)
        if source:
            dest = OUT_DIR / disk_name
            dest.write_bytes(Path(source).read_bytes())
            built[package] = str(dest)
    return built


def build_sample_packages() -> None:
    """Build the sample `.lzp` packages the image ships (`/system/share/samples/pkgdemo.lzp`).

    They are made from the apps just built (`tools/pkg/build_samples.py`) and
    embedded by the root `build.rs`. Output goes to stderr: stdout is the JSON
    result of this script. A failure only warns; the apps themselves built.
    """
    sys.path.insert(0, str(ROOT / "tools" / "pkg"))
    try:
        import build_samples

        build_samples.build_sample_all(OUT_DIR, ROOT / "target" / "pkg")
    except Exception as error:  # noqa: BLE001 - a sample must never fail the app build
        print(f"warning: sample packages not built: {error}", file=sys.stderr)


def build_core_packages() -> bool:
    """Package every desktop app as a core `.lzp` (issue #509,
    `tools/xui/core_packages.py`): `target/pkg/core/<sn>-<version>.lzp`, which
    the root `build.rs` embeds in `/system/packages`. Unlike a sample, a core
    package that cannot be built fails the build: the desktop would lack the
    app. Output goes to stderr (stdout is this script's JSON result)."""
    import core_packages

    try:
        for archive in core_packages.build_core_packages(OUT_DIR, ROOT / "target" / "pkg" / "core"):
            print(f"core package: {archive}", file=sys.stderr)
    except core_packages.CoreError as error:
        print(f"error: core packages not built: {error}", file=sys.stderr)
        return False
    return True


def main() -> int:
    """Parse arguments, build the apps and report them as JSON; return the
    process exit code."""
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument("--debug", action="store_true", help="build the debug profile")
    parser.add_argument("--no-core-packages", action="store_true",
                        help="skip packaging the desktop apps (run tools/xui/core_packages.py later)")
    parser.add_argument("--no-lazyweb", action="store_true",
                        help="skip LazyWeb (the NetSurf browser, the slowest zig build)")
    parser.add_argument("--mail", action="store_true",
                        help="also build Mail (xui-mail.elf, with zig); LAZYOS_MAIL=1 does the same")
    args = parser.parse_args()

    OUT_DIR.mkdir(parents=True, exist_ok=True)
    built: dict[str, str] = {}

    if not ensure_target():
        print(json.dumps(built))
        return 0

    profile = "debug" if args.debug else "release"
    command = [
        "cargo",
        "build",
        "--manifest-path",
        str(APP / "Cargo.toml"),
        "--target",
        TARGET,
    ]
    if not args.debug:
        command.append("--release")
    build = run(command, env=build_env(), stream=True)
    if build.returncode != 0:
        # Only a missing toolchain/target is a skip (handled above); a real
        # compile error must fail CI instead of silently skipping the run.
        print("error: xui app build failed", file=sys.stderr)
        print(json.dumps(built))
        return 1

    release = APP / "target" / TARGET / profile
    for name, disk_name in BINS.items():
        source = release / name
        if not source.is_file():
            continue
        dest = OUT_DIR / disk_name
        dest.write_bytes(source.read_bytes())
        built[name] = str(dest)

    mail = args.mail or os.environ.get("LAZYOS_MAIL") == "1"
    built.update(build_zig_apps(args.debug, lazyweb=not args.no_lazyweb, mail=mail))
    if mail and MAIL_PACKAGE not in built:
        # Asked for by name: an image without it is not what was requested.
        print(f"error: {MAIL_PACKAGE} was requested but not built", file=sys.stderr)
        return 1

    build_sample_packages()
    if not args.no_core_packages and not build_core_packages():
        print(json.dumps(built, indent=2))
        return 1
    print(json.dumps(built, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
