"""The optional build steps `run_demo.py` runs before `cargo build`.

Each runs one tool script quietly and reports a failure in one line; the
explicitly requested ones (`--lazyrad`, `--doom`, `--quake`, `--srb2kart`, `--emusic`, `--modplayer`,
`--pictures`, `--linuxapps`, `--tls`, `--installer`, `--mail`, `--devices`, `--wifi`) return
False so the run stops instead of booting an image without what was asked
for.
"""

from __future__ import annotations

import os
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def run_tool(what: str, script: str, args: tuple[str, ...] = (), announce: bool = True) -> bool:
    """Run `tools/<script>` with `args` from the repo root; True on success."""
    if announce:
        print(f"building {what} (tools/{script})…", flush=True)
    command = [sys.executable, str(ROOT / "tools" / script), *args]
    return subprocess.run(command, cwd=ROOT, stdout=subprocess.DEVNULL).returncode == 0


def required(what: str, script: str, args: tuple[str, ...] = ()) -> bool:
    """[`run_tool`] for a step the user asked for: a failure is an error."""
    if run_tool(what, script, args):
        return True
    print(f"error: {what} did not build (run `python tools/{script}`)", file=sys.stderr)
    return False


def build_rhai() -> None:
    """Rebuild `target/rhai/rhai.elf` so the image never embeds a stale or
    missing `rhai` (issue #319). Optional: a host without the musl target
    still boots, just without the command, which `build.py` explains."""
    if not run_tool("rhai", "rhai/build.py"):
        print("warning: rhai did not build; the image will have no `rhai` command",
              file=sys.stderr)


def build_lazyrad() -> bool:
    """The LazyRAD IDE and player (`--lazyrad`)."""
    return required("LazyRAD", "lazyrad/build.py")


def build_pictures() -> bool:
    """The Picture Viewer (`--pictures`): the LazyRAD player it runs on.
    `tools/lazyrad/build.py` repackages the core packages after it, and the
    desktop build packages them again before the image, so `os.lazy.pictures`
    (the player, `lazyrad-os/samples/pictures` and the sample pictures) is
    there either way."""
    return required("the Picture Viewer (the LazyRAD player)", "lazyrad/build.py")


def build_doom() -> bool:
    """The Doom package (`tools/doom/build.py`: engine, Freedoom, then
    `target/pkg/doom.lzp`); a missing toolchain or download stops the run."""
    return required("Doom", "doom/build.py", ("--require",))


def build_quake() -> bool:
    """The Quake package (`tools/quake/build.py`: the quake-srp engine and
    its LazyOS bridge, id's shareware pak, then `target/pkg/quake.lzp`); a
    missing toolchain or download stops the run."""
    return required("Quake", "quake/build.py", ("--require",))


def build_srb2kart() -> bool:
    """SRB2Kart (`tools/srb2kart/build.py`: the Kart-Public engine and its
    LazyOS platform layer, Kart Krew's game data, then `target/pkg/srb2kart.lzp`
    and `target/srb2kart/data/`); a missing toolchain or download stops the run."""
    return required("SRB2Kart", "srb2kart/build.py", ("--require",))


def build_emusic() -> bool:
    """The emusic package (`tools/emusic/build.py`: the program, then
    `target/pkg/emusic.lzp`); `EMUSIC_SRC` names a local emusic clone. A
    missing toolchain or download stops the run."""
    return required("emusic", "emusic/build.py", ("--require",))


def build_modplayer() -> bool:
    """The LazyRAD MOD player package (`target/pkg/MODPLAY.LZP` from
    `lazyrad-os/samples/modplayer`), after LazyRAD itself."""
    return required("the MOD player package", "lazyrad/package.py", ("--no-build", "--require"))


def build_linuxapps() -> bool:
    """The Linux command-line programs (dash, sqlite3, jq, rg from pinned
    sources); a missing toolchain or download stops the run."""
    return required("the Linux programs", "linuxapps/build.py", ("--require",))


def build_jsc() -> bool:
    """`jsc`, `jsc-jit` (WebKit's JSCOnly port) and `cxxproof`, built in Docker by
    `tools/jsc/build.py`; a missing Docker or download stops the run."""
    return required("JavaScriptCore", "jsc/build.py", ("--require", "--jit"))


def build_webkit_deps() -> bool:
    """The WebKit dependency bundle and `depsproof`, built in Docker by
    `tools/webkit/deps.py`; a missing Docker or download stops the run."""
    return required("the WebKit dependency bundle", "webkit/deps.py", ("--require",))


def build_webkit_port() -> bool:
    """The WebKit port (`WebProcess`, `NetworkProcess`, `webkit-smoke`), built in
    Docker by `tools/webkit/port.py` against the dependency bundle (built
    first); a missing Docker or download stops the run."""
    return (required("the WebKit dependency bundle", "webkit/deps.py", ("--require",))
            and required("the WebKit port", "webkit/port.py", ("--require",)))


def build_webkit_processes() -> bool:
    """The bundle and the two engine processes a browser starts (`WebProcess`,
    `NetworkProcess`), in Docker. Not `webkit-smoke` and its host reference run,
    which only the console proof (`--webkit-port`) judges: they cost a program
    of 107 MiB to strip and a headless page load in the container on every run."""
    return (required("the WebKit dependency bundle", "webkit/deps.py", ("--require",))
            and required("the WebKit engine processes", "webkit/port.py",
                         ("--require", "--only", "WebProcess", "NetworkProcess")))


def build_lazyweb() -> bool:
    """LazyWeb (`--lazyweb`): a plain xui app, a client of webd (docs/webd-plan.md P4), so what it
    builds is webd's: the bundle and the port's processes, then webd's service library and program
    (`tools/webkit/webd.py`), all in Docker, then the xui apps, whose build packages `xui-lazyweb.elf`
    as the core package `os.lazy.lazyweb` (and `xui-webd.elf`); a missing Docker or download stops the
    run."""
    return (build_webkit_processes()
            and required("webd", "webkit/webd.py", ("--require",))
            and build_xui_apps())


def build_webd() -> bool:
    """webd (`--webd`): the bundle and the port's processes, then webd's service library and
    the program (`tools/webkit/webd.py`), all in Docker, then the xui apps, whose build packages
    `xui-webd.elf` as the core package `os.lazy.webd` (and the demo client, when the tree has it);
    a missing Docker or download stops the run."""
    return (build_webkit_processes()
            and required("webd", "webkit/webd.py", ("--require",))
            and build_xui_apps())


#: What `LAZYOS_WEBD` embeds (`build_support/webd_embed.rs`, `linuxapps_embed.rs`).
WEBD_ARTIFACTS = (ROOT / "target" / "xui" / "xui-webd.elf", ROOT / "target" / "webkit" / "bin" / "WebProcess",
                  ROOT / "target" / "webkit" / "bin" / "NetworkProcess")


def webd_built() -> bool:
    """webd's programs are all there: a desktop run uses them as they are (a Docker build of WebKit is
    minutes even when nothing changed); `--webd` rebuilds."""
    return all(path.is_file() for path in WEBD_ARTIFACTS)


def webd_env(args, env: dict, built, build) -> None:
    """The `LAZYOS_WEBD` and `LAZYOS_WEBDDEMO` switches of a run. Every desktop ships webd (Docs and Mail need it):
    built when its programs are missing, and left out (the apps then say the web engine is not running)
    when that fails, as with no Docker; `--no-webd` leaves it out, `--webd` (built by the caller) requires it, and
    so does `--lazyweb` (which builds it itself and refuses `--no-webd`)."""
    if args.no_webd:
        env["LAZYOS_WEBD"] = "0"
    elif getattr(args, "lazyweb", False):
        env["LAZYOS_WEBD"] = "1"
    elif getattr(args, "webd_implicit", False):
        if built() or build():
            env["LAZYOS_WEBD"] = "1"
        else:
            print("warning: webd could not be built (Docker?): this desktop has none, so Docs and Mail "
                  "cannot show their pages; `--no-webd` silences this", file=sys.stderr)
            env["LAZYOS_WEBD"] = "0"
            args.webd_implicit = False  # the volume need not hold its programs
    if args.webd_demo:
        env["LAZYOS_WEBDDEMO"] = "1"


def build_tls() -> bool:
    """`fetch`, also run as `curl` and `wget` (rustls over pure-Rust RustCrypto,
    built by `tools/nettls/build.py`); a failed build stops the run."""
    return required("the HTTPS tools", "nettls/build.py", ("--require",))


def build_installer() -> bool:
    """`lazyos-install` (`--installer`), the static musl program the stick
    carries to install itself beside Linux (`tools/install/build.py`)."""
    return required("lazyos-install", "install/build.py", ("--require",))


def build_wifi() -> bool:
    """The Wi-Fi firmware (`--wifi`): `tools/firmware/fetch.py` downloads the
    pinned linux-firmware files into `target/firmware` (cached once
    verified); `wifid` itself is a `user` binary the image build compiles."""
    return required("the Wi-Fi firmware", "firmware/fetch.py", ("--switch", "wifi"))


#: LazySky's client binary (`tools/xui/build.py` builds it with the other apps).
LAZYSKY_ELF = ROOT / "target" / "xui" / "xui-lazysky.elf"


def build_lazysky() -> bool:
    """LazySky (`--lazysky`): the xui apps when its binary is missing."""
    if LAZYSKY_ELF.is_file() or (build_xui_apps() and LAZYSKY_ELF.is_file()):
        return True
    print(f"error: {LAZYSKY_ELF} was not built "
          "(run `python tools/xui/build.py` and read its errors)", file=sys.stderr)
    return False


#: The developer tools' programs (`build_support/devtools_embed.rs` TOOLS); `tools/xui/build.py`
#: builds them with the other apps.
DEVTOOLS_APPS = [ROOT / "target" / "xui" / "xui-playground.elf"]
RHAI_ELF = ROOT / "target" / "rhai" / "rhai.elf"


def build_devtools() -> bool:
    """The developer tools (`--devtools`, docs/devtool-packages-plan.md): the xui apps when a
    tool's binary is missing, and `rhai`, which the image build refuses to go without (a tool
    runs the user's scripts; the soft `build_rhai` only warns)."""
    explicit = Path(os.environ.get("LAZYOS_RHAI", ""))  # `rhai_embed::find`: a file, else the build
    if not (explicit.is_file() or RHAI_ELF.is_file()) and not required("rhai", "rhai/build.py"):
        return False
    if all(app.is_file() for app in DEVTOOLS_APPS) or (
            build_xui_apps() and all(app.is_file() for app in DEVTOOLS_APPS)):
        return True
    print(f"error: {DEVTOOLS_APPS[0]} was not built "
          "(run `python tools/xui/build.py` and read its errors)", file=sys.stderr)
    return False


def build_xui_apps() -> bool:
    """The desktop's xui apps, which include the Devices app (`--devices`)."""
    return required("the xui apps", "xui/build.py")


def build_mail() -> bool:
    """The xui apps plus Mail (esMail with its IMAP/SMTP core and SQLite,
    built with zig; docs/mail.md) and their core packages."""
    return required("Mail", "xui/build.py", ("--mail",))
