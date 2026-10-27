"""The build steps of the optional apps an image can embed (LazyRAD, the MOD
player package, the Picture Viewer, Doom, the Linux programs, the HTTPS tools, Mail,
the Wi-Fi firmware, the stick's `lazyos-install`),
run before `cargo build`, and the switches of the opt-in desktop apps (the tray demo, the
Picture Viewer).
`catalog` re-exports them; they live apart to keep it small."""

from __future__ import annotations

import sys

from .scriptenv import script_env
from .scripts import SCRIPTS
from . import osvolume

PY = sys.executable


def lazyrad_step(cfg: dict) -> list[dict]:
    """The step that builds LazyRAD's static-musl ELFs, when the image embeds them
    (the Picture Viewer runs on the player, `lrplay`)."""
    if not cfg.get("lazyrad") and not cfg.get("modplayer") and not wants_pictures(cfg):
        return []
    return [{"label": "Build LazyRAD (static musl)",
             "argv": [PY, "tools/lazyrad/build.py"]}]


def doom_step(cfg: dict) -> list[dict]:
    """The step that builds the Doom package, when the image embeds it."""
    if not cfg.get("doom"):
        return []
    return [{"label": "Build Doom package (engine + Freedoom)",
             "argv": [PY, "tools/doom/build.py", "--require"]}]


def quake_step(cfg: dict) -> list[dict]:
    """The step that builds the Quake package (engine + id's shareware pak),
    when the image embeds it."""
    if not cfg.get("quake"):
        return []
    return [{"label": "Build Quake package (engine + shareware pak)",
             "argv": [PY, "tools/quake/build.py", "--require"]}]


def srb2kart_step(cfg: dict) -> list[dict]:
    """The step that builds SRB2Kart (engine package + Kart Krew's game data),
    when the image embeds it."""
    if not cfg.get("srb2kart"):
        return []
    return [{"label": "Build SRB2Kart (engine package + fetched game data)",
             "argv": [PY, "tools/srb2kart/build.py", "--require"]}]


def emusic_step(cfg: dict) -> list[dict]:
    """The step that builds the emusic package, when the image embeds it."""
    if not cfg.get("emusic"):
        return []
    return [{"label": "Build emusic package",
             "argv": [PY, "tools/emusic/build.py", "--require"]}]


def modplayer_step(cfg: dict) -> list[dict]:
    """The step that packages the LazyRAD MOD player, when the image embeds it
    (after `lazyrad_step`: the package carries the player it built)."""
    if not cfg.get("modplayer"):
        return []
    return [{"label": "Package the MOD player (LazyRAD)",
             "argv": [PY, "tools/lazyrad/package.py", "--no-build", "--require"]}]


def linuxapps_step(cfg: dict) -> list[dict]:
    """The step that builds the Linux command-line programs, when embedded."""
    if not cfg.get("linuxapps"):
        return []
    return [{"label": "Build Linux programs (dash, sqlite3, jq, rg)",
             "argv": [PY, "tools/linuxapps/build.py", "--require"]}]


def rustc_step(cfg: dict) -> list[dict]:
    """The step that cross-compiles the Rust compiler (in Docker), when embedded."""
    if not cfg.get("rustc"):
        return []
    return [{"label": "Build rustc for LazyOS (Docker; half an hour the first time)",
             "argv": [PY, "tools/selfhost/build.py", "--require"]}]


def jsc_step(cfg: dict) -> list[dict]:
    """The step that builds JavaScriptCore's `jsc` (in Docker), when embedded."""
    if not cfg.get("jsc"):
        return []
    return [{"label": "Build jsc, JavaScriptCore's shell (Docker)",
             "argv": [PY, "tools/jsc/build.py", "--require", "--jit"]}]


def webkit_deps_step(cfg: dict) -> list[dict]:
    """The step that builds the WebKit dependency bundle and `depsproof` (in
    Docker, tools/webkit/deps.py), when embedded."""
    if not cfg.get("webkit_deps"):
        return []
    return [{"label": "Build the WebKit dependency bundle and depsproof (Docker)",
             "argv": [PY, "tools/webkit/deps.py", "--require"]}]


def webkit_port_step(cfg: dict) -> list[dict]:
    """The step that builds the WebKit port (WebProcess, NetworkProcess,
    webkit-smoke; in Docker, tools/webkit/port.py), when its console proof is
    embedded. It needs the dependency bundle, which `port.py` itself does not build."""
    if not cfg.get("webkit_port"):
        return []
    return [{"label": "Build the WebKit dependency bundle (Docker)",
             "argv": [PY, "tools/webkit/deps.py", "--require"]},
            {"label": "Build the WebKit port: WebProcess, NetworkProcess, webkit-smoke (Docker)",
             "argv": [PY, "tools/webkit/port.py", "--require"]}]


def lazyweb_step(cfg: dict) -> list[dict]:
    """The steps that build LazyWeb (docs/lazyweb.md), a plain xui app and a client of webd: the
    bundle, the port's processes and webd (the service as a static library and the program, in Docker),
    then the xui apps, which package the browser as `os.lazy.lazyweb` and webd as `os.lazy.webd`."""
    if not cfg.get("lazyweb"):
        return []
    return [{"label": "Build the WebKit dependency bundle (Docker)",
             "argv": [PY, "tools/webkit/deps.py", "--require"]},
            {"label": "Build the WebKit port: WebProcess, NetworkProcess (Docker)",
             "argv": [PY, "tools/webkit/port.py", "--require", "--only", "WebProcess", "NetworkProcess"]},
            {"label": "Build webd, which LazyWeb draws its pages with: the service and the program (Docker)",
             "argv": [PY, "tools/webkit/webd.py", "--require"]},
            {"label": "Build xui apps with LazyWeb", "argv": [PY, "tools/xui/build.py"]}]


def webd_step(cfg: dict) -> list[dict]:
    """The steps that build webd (docs/webd.md): the bundle, the port's processes and the daemon
    (the service as a static library and the program, in Docker), then the xui apps, which package
    it as `os.lazy.webd`. Only when asked for: every desktop ships webd once it has been built (Docs and Mail
    need it), but a launch does not start a Docker build of WebKit by itself (`run_demo --desktop` does, and
    goes on without it when that fails)."""
    # LazyWeb's steps build the bundle, the engine processes and webd itself.
    if not osvolume.requires_webd(cfg) or cfg.get("lazyweb"):
        return []
    shared = [
        {"label": "Build the WebKit dependency bundle (Docker)",
         "argv": [PY, "tools/webkit/deps.py", "--require"]},
        {"label": "Build the WebKit port: WebProcess, NetworkProcess (Docker)",
         "argv": [PY, "tools/webkit/port.py", "--require", "--only", "WebProcess", "NetworkProcess"]}]
    return shared + [
            {"label": "Build webd: the service and the program (Docker)",
             "argv": [PY, "tools/webkit/webd.py", "--require"]},
            {"label": "Build xui apps with webd", "argv": [PY, "tools/xui/build.py"]}]


def tls_step(cfg: dict) -> list[dict]:
    """The step that builds the HTTPS clients (`fetch`, `curl`, `wget`), when embedded."""
    if not cfg.get("tls"):
        return []
    return [{"label": "Build HTTPS tools (curl, wget, fetch)",
             "argv": [PY, "tools/nettls/build.py", "--require"]}]


def wifi_step(cfg: dict) -> list[dict]:
    """The step that fetches the Wi-Fi firmware (pinned, hash-checked), when
    the image carries Wi-Fi; the build fails without it."""
    if not cfg.get("wifi"):
        return []
    return [{"label": "Fetch the Wi-Fi firmware (linux-firmware, pinned)",
             "argv": [PY, "tools/firmware/fetch.py", "--switch", "wifi"]}]


def lazysky_step(cfg: dict) -> list[dict]:
    """The step that builds the LazySky client (with the other xui apps and
    their core packages), when the image embeds it."""
    if not cfg.get("lazysky"):
        return []
    return [{"label": "Build xui apps with LazySky", "argv": [PY, "tools/xui/build.py"]}]


def mail_step(cfg: dict) -> list[dict]:
    """The step that builds Mail (esMail; docs/mail.md) with the other xui apps
    and repackages them, when a desktop image embeds it."""
    if not wants_mail(cfg):
        return []
    return [{"label": "Build Mail (esMail, with zig for SQLite) and the core packages",
             "argv": [PY, "tools/xui/build.py", "--mail"]}]


def wants_mail(cfg: dict) -> bool:
    """Mail is a desktop app: it means nothing on a CLI image."""
    return bool(cfg.get("mail") and cfg.get("desktop"))


def mail_env(cfg: dict) -> dict[str, str]:
    """`LAZYOS_MAIL=1` (the image embeds `xui-mail.elf`); `build_env` also turns
    on the HTTPS stack, since `cfg["tls"]` follows `mail`."""
    return {"LAZYOS_MAIL": "1"} if wants_mail(cfg) else {}


def mail_argv(cfg: dict) -> list[str]:
    """run_demo's `--mail`, which builds Mail and sets the switches itself."""
    return ["--mail"] if wants_mail(cfg) and not cfg.get("skip_build") else []


def wants_traydemo(cfg: dict) -> bool:
    """The tray demo (docs/tray-plan.md T1) is a desktop app like Mail; unlike
    Mail it needs no build step: `tools/xui/build.py` always builds it."""
    return bool(cfg.get("traydemo") and cfg.get("desktop"))


def wants_games(cfg: dict) -> bool:
    """LazyGolf and the NES Player are desktop apps like the tray demo:
    `tools/xui/build.py` always builds them, so there is no build step."""
    return bool(cfg.get("games") and cfg.get("desktop"))


def wants_devtools(cfg: dict) -> bool:
    """The developer tools (docs/devtool-packages-plan.md) are desktop apps like the tray demo:
    `tools/xui/build.py` always builds them, so the only step is `rhai`, which they run scripts with."""
    return bool(cfg.get("devtools") and cfg.get("desktop"))


def devtools_step(cfg: dict) -> list[dict]:
    """The step that builds `rhai` when the image ships the developer tools, ticked or asked for by
    the chosen session script (the image build refuses `LAZYOS_DEVTOOLS=1` without the command)."""
    if not (wants_devtools(cfg) or script_env(cfg, SCRIPTS).get("LAZYOS_DEVTOOLS")):
        return []
    return [{"label": "Build rhai (the developer tools' script engine)",
             "argv": [PY, "tools/rhai/build.py"]}]


def wants_pictures(cfg: dict) -> bool:
    """The Picture Viewer (docs/lazyrad-pictures.md) is a desktop app; its build
    step is LazyRAD's (`lazyrad_step`), which packages it with the others."""
    return bool(cfg.get("pictures") and cfg.get("desktop"))


def desktop_app_env(cfg: dict) -> dict[str, str]:
    """The opt-in desktop apps' switches: `LAZYOS_MAIL`, `LAZYOS_TRAYDEMO`,
    `LAZYOS_GAMES`, `LAZYOS_DEVTOOLS`, `LAZYOS_PICTURES`, `LAZYOS_JSC` (jsc and cxxproof, built in Docker by
    `tools/jsc/build.py`; a console program, so desktop or not), `LAZYOS_LAZYVM` (LazyVM's stage, written by LazyVM), `LAZYOS_RUSTC` (the Rust compiler,
    `tools/selfhost/build.py`, a console program too) and
    `LAZYOS_WEBKIT_DEPS` (depsproof, `tools/webkit/deps.py`) and `LAZYOS_WEBKIT_PORT`
    (the W4 console proof, `tools/webkit/port.py`). LazyWeb, `LAZYOS_LAZYWEB`, is a
    desktop app like LazySky: it brings the desktop profile (`catalog.build_env`) and the stack,
    and `run_demo --lazyweb` builds it and sets the big-app profile and the OS volume size. It needs webd,
    which the image build then ships whatever `LAZYOS_WEBD` says unset, so `no_webd` is ignored with it."""
    return (mail_env(cfg) | ({"LAZYOS_TRAYDEMO": "1"} if wants_traydemo(cfg) else {})
            | ({"LAZYOS_GAMES": "1"} if wants_games(cfg) else {})
            | ({"LAZYOS_DEVTOOLS": "1"} if wants_devtools(cfg) else {})
            | ({"LAZYOS_PICTURES": "1"} if wants_pictures(cfg) else {})
            | ({"LAZYOS_JSC": "1"} if cfg.get("jsc") else {})
            | ({"LAZYOS_LAZYVM": "1"} if cfg.get("lazyvm") else {})
            | ({"LAZYOS_RUSTC": "1"} if cfg.get("rustc") else {})
            | ({"LAZYOS_WEBKIT_DEPS": "1"} if cfg.get("webkit_deps") else {})
            | ({"LAZYOS_WEBKIT_PORT": "1"} if cfg.get("webkit_port") else {})
            | ({"LAZYOS_LAZYWEB": "1"} if cfg.get("lazyweb") else {})
            | ({"LAZYOS_WEBD": "1"} if osvolume.requires_webd(cfg) else {})
            | ({"LAZYOS_WEBD": "0"} if osvolume.leaves_webd_out(cfg) else {})
            | ({"LAZYOS_WEBDDEMO": "1"} if cfg.get("webd_demo") else {}))


def desktop_app_argv(cfg: dict) -> list[str]:
    """run_demo's `--mail`, `--traydemo`, `--games`, `--devtools`, `--pictures`, `--jsc`, `--rustc` and `--webkit-port`,
    which set those switches themselves, and `--lazyweb` and `--webd`."""
    building = not cfg.get("skip_build")
    return (mail_argv(cfg) + (["--traydemo"] if wants_traydemo(cfg) and building else [])
            + (["--games"] if wants_games(cfg) and building else [])
            + (["--devtools"] if wants_devtools(cfg) and building else [])
            + (["--pictures"] if wants_pictures(cfg) and building else [])
            + (["--jsc"] if cfg.get("jsc") and building else [])
            + (["--lazyvm"] if cfg.get("lazyvm") and building else [])
            + (["--rustc"] if cfg.get("rustc") and building else [])
            + (["--webkit-deps"] if cfg.get("webkit_deps") and building else [])
            + (["--webkit-port"] if cfg.get("webkit_port") and building else [])
            + (["--lazyweb"] if cfg.get("lazyweb") and building else [])
            + (["--webd"] if cfg.get("webd") and building else [])
            + (["--webd-demo"] if cfg.get("webd_demo") and building else [])
            + (["--no-webd"] if osvolume.leaves_webd_out(cfg) and building else []))


def installer_step(cfg: dict) -> list[dict]:
    """The step that builds `lazyos-install`, when the stick carries it
    (docs/grub-install-plan.md I2)."""
    if not cfg.get("installer"):
        return []
    return [{"label": "Build lazyos-install (static musl)",
             "argv": [PY, "tools/install/build.py", "--require"]}]


def installer_argv(cfg: dict) -> list[str]:
    """run_demo's `--installer`, which builds it and sets the stick switches."""
    return ["--installer"] if cfg.get("installer") and not cfg.get("skip_build") else []


def app_steps(cfg: dict) -> list[dict]:
    """Every optional app the image embeds, built before `cargo build`."""
    return (lazyrad_step(cfg) + modplayer_step(cfg) + doom_step(cfg) + quake_step(cfg)
            + srb2kart_step(cfg) + emusic_step(cfg)
            + linuxapps_step(cfg) + rustc_step(cfg)
            + jsc_step(cfg) + webkit_deps_step(cfg) + webkit_port_step(cfg)
            + lazyweb_step(cfg) + webd_step(cfg) + tls_step(cfg) + lazysky_step(cfg) + mail_step(cfg)
            + devtools_step(cfg)
            + wifi_step(cfg) + installer_step(cfg))
