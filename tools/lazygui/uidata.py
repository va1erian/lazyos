"""The Advanced tab's snapshot (`Launcher._advanced_cfg`, split out of `ui.py`
to keep it small): the dict `catalog.build_env`/`build_plan` reads."""

from __future__ import annotations

from .catalog import SCRIPTS


def advanced_cfg(ui) -> dict:
    """Snapshot the Advanced controls of `ui` into the dict `catalog.build_plan`
    expects (`Simple` mode feeds the same dict into `simple_config`)"""
    names = [n for n in SCRIPTS if n[1] == ui.v["script"].get()]
    return {
        "mode": ui.v["mode"].get(),
        "profile": ui.v["profile"].get(),
        "accel": ui.v["accel"].get(),
        "disk": ui.v["disk"].get(),
        "home_path": ui.v["home_path"].get().strip(),
        "home_disk": ui.v["home_disk"].get(),
        "data_path": ui.v["data_path"].get().strip(),
        "data_disk": ui.v["data_disk"].get(),
        "reset_os": ui.v["reset_os"].get(),
        "memory": ui.v["memory"].get().strip(),
        "limits": ui.v["limits"].get().strip(),
        "display_mode": ui.v["display_mode"].get().strip(),
        "assets": ui.v["assets"].get().strip(),
        "autologin": ui.v["autologin"].get().strip(), "setup": ui.v["setup"].get(),
        "times": ui.v["times"].get().strip(),
        "timeout": ui.v["timeout"].get().strip(),
        "abi_time": ui.v["abi_time"].get().strip(),
        "abi_only": ui.v["abi_only"].get().strip(),
        "extra": ui.v["extra"].get().strip(),
        "out": ui.v["out"].get().strip(),
        "qemu": ui.v["qemu"].get().strip(),
        "busybox": ui.v["busybox"].get().strip(),
        "skip_build": ui.v["skip_build"].get(),
        "headless": ui.v["headless"].get(),
        "tablet": ui.v["tablet"].get(),
        "sound": ui.v["sound"].get(),
        "sound_card": ui.v["sound_card"].get(),
        "nic": ui.v["nic"].get(),
        **{key: ui.v[key].get() for key in ("devd", "irqchip", "msi")},
        "abi_build": ui.v["abi_build"].get(),
        "desktop": ui.v["desktop"].get(),
        "services": ui.v["services"].get(),
        "xuid": ui.v["xuid"].get(),
        "shellprobe": ui.v["shellprobe"].get(),
        "msgctl": ui.v["msgctl"].get(),
        "msgrd": ui.v["msgrd"].get(),
        "xui_client": ui.v["xui_client"].get(),
        "xui_app": ui.v["xui_app"].get(),
        "xui_autostart": ui.v["xui_autostart"].get(),
        "lazyrad": ui.v["lazyrad"].get(),
        "usb_image": ui.v["usb_image"].get(),
        "shell": ui.v["shell"].get(),
        "lazyrad_samples": ui.v["lazyrad_samples"].get().strip(),
        "devices": ui.v["devices"].get(),
        "doom": ui.v["doom"].get(), "quake": ui.v["quake"].get(),
        "emusic": ui.v["emusic"].get(),
        "modplayer": ui.v["modplayer"].get(),
        "net": ui.v["net"].get(),
        "net_forwards": ui.v["net_forwards"].get().strip(),
        "net_restrict": ui.v["net_restrict"].get(),
        "linuxapps": ui.v["linuxapps"].get(),
        **{name: ui.v[name].get() for name in ("smb", "dbgd", "dbgd_control")},
        # Mail speaks TLS: its switch brings the HTTPS stack and the card.
        "tls": ui.v["tls"].get() or ui.v["mail"].get(),
        "journal": ui.v["journal"].get(),
        **{app: ui.v[app].get() for app in ("lazyweb", "mail", "traydemo", "pictures")},
        "script": SCRIPTS.index(names[0]) if names else 0,
    }
