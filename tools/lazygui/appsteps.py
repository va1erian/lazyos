"""The build steps of the optional apps an image can embed (LazyRAD, the MOD
player package, the Picture Viewer, Doom, the Linux programs, the HTTPS tools, LazyWeb, Mail,
the Wi-Fi firmware, the stick's `lazyos-install`),
run before `cargo build`, and the switches of the opt-in desktop apps (the tray demo, the
Picture Viewer).
`catalog` re-exports them; they live apart to keep it small."""

from __future__ import annotations

import sys

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


def jsc_step(cfg: dict) -> list[dict]:
    """The step that builds JavaScriptCore's `jsc` (in Docker), when embedded."""
    if not cfg.get("jsc"):
        return []
    return [{"label": "Build jsc, JavaScriptCore's shell (Docker)",
             "argv": [PY, "tools/jsc/build.py", "--require"]}]


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


def lazyweb_step(cfg: dict) -> list[dict]:
    """The step that builds the LazyWeb browser (with the other xui apps and
    their core packages), when the image embeds it."""
    if not cfg.get("lazyweb"):
        return []
    return [{"label": "Build xui apps with LazyWeb", "argv": [PY, "tools/xui/build.py"]}]
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


def wants_pictures(cfg: dict) -> bool:
    """The Picture Viewer (docs/lazyrad-pictures.md) is a desktop app; its build
    step is LazyRAD's (`lazyrad_step`), which packages it with the others."""
    return bool(cfg.get("pictures") and cfg.get("desktop"))


def desktop_app_env(cfg: dict) -> dict[str, str]:
    """The opt-in desktop apps' switches: `LAZYOS_MAIL`, `LAZYOS_TRAYDEMO`,
    `LAZYOS_PICTURES`, and `LAZYOS_JSC` (jsc and cxxproof, built in Docker by
    `tools/jsc/build.py`; a console program, so desktop or not)."""
    return (mail_env(cfg) | ({"LAZYOS_TRAYDEMO": "1"} if wants_traydemo(cfg) else {})
            | ({"LAZYOS_PICTURES": "1"} if wants_pictures(cfg) else {})
            | ({"LAZYOS_JSC": "1"} if cfg.get("jsc") else {}))


def desktop_app_argv(cfg: dict) -> list[str]:
    """run_demo's `--mail`, `--traydemo`, `--pictures` and `--jsc`, which set
    those switches themselves."""
    building = not cfg.get("skip_build")
    return (mail_argv(cfg) + (["--traydemo"] if wants_traydemo(cfg) and building else [])
            + (["--pictures"] if wants_pictures(cfg) and building else [])
            + (["--jsc"] if cfg.get("jsc") and building else []))


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
            + linuxapps_step(cfg)
            + jsc_step(cfg)
            + tls_step(cfg) + lazyweb_step(cfg) + lazysky_step(cfg) + mail_step(cfg)
            + wifi_step(cfg) + installer_step(cfg))
