"""The iPXE scripts of the Q0 spike, as pure functions (docs/netboot-plan.md §5, §7).

`serve.py` serves them (`/boot.ipxe`, `/build.ipxe`, `/sanboot.ipxe`);
`fetch_ipxe.py` writes `autoexec.ipxe` and an example menu from the same
templates, so what the tests pin is what the guest runs.
"""

from __future__ import annotations

import common

#: Menu timeout the plan's §5 uses, in milliseconds.
DEFAULT_TIMEOUT_MS = 5000
#: The menu item id of the one build (iPXE item ids are words).
ITEM_ID = "b_spike"


def registry_url(host: str, port: int) -> str:
    """`http://host:port`, without `:80`."""
    return f"http://{host}" if port == 80 else f"http://{host}:{port}"


def file_url(reg: str, name: str) -> str:
    return f"{reg}/files/{name}"


def boot_menu(reg: str, label: str, timeout_ms: int = DEFAULT_TIMEOUT_MS) -> str:
    """`/boot.ipxe`: the plan's menu with one build, a local row and a shell row.

    Choosing the build chains `/build.ipxe`. A chain that returns (the loader
    came back, or a fetch failed) lands in the iPXE shell so the images can
    still be inspected (`imgstat`); Esc at the menu exits to the firmware.
    """
    return "\n".join([
        "#!ipxe",
        f"set reg {reg}",
        ":menu",
        "menu LazyOS netboot spike on ${net0/mac}",
        "item --gap -- spike",
        f"item {ITEM_ID} {label}",
        "item --gap",
        "item local  Boot the local copy (exit to the firmware)",
        "item shell  iPXE shell",
        f"choose --default {ITEM_ID} --timeout {timeout_ms} target || goto local",
        "goto ${target}",
        f":{ITEM_ID}",
        "chain ${reg}/build.ipxe || goto menu",
        "goto shell",
        ":shell",
        "shell",
        "goto menu",
        ":local",
        "exit",
        "",
    ])


def build_script(reg: str) -> str:
    """`/build.ipxe`: fetch the three loose files, then chain the loader.

    Every URL is absolute, so the script also works when typed by hand
    (`chain <reg>/build.ipxe`) without `${reg}` set.
    """
    lines = ["#!ipxe"]
    for name in common.PAYLOAD_FILES:
        lines.append(f"imgfetch --name {name} {file_url(reg, name)}")
    lines.append(f"chain --name {common.LOADER_NAME} {file_url(reg, common.LOADER_NAME)}")
    lines.append("")
    return "\n".join(lines)


def sanboot_script(reg: str, image: str = "esp.img") -> str:
    """`/sanboot.ipxe`: the fallback, the ESP-only disk image as a SAN disk."""
    return "\n".join(["#!ipxe", f"sanboot --no-describe {file_url(reg, image)}", ""])


def autoexec(reg: str, on_fail: str = "shell", fail_sleep: int = 5) -> str:
    """`autoexec.ipxe`: DHCP, then the menu; `on_fail` is `shell` or `exit`.

    `exit` is the plan's §7 file for the box (back to the firmware, which
    boots its next entry); `shell` is the spike's, so a failure stays on screen.
    """
    if on_fail not in ("shell", "exit"):
        raise ValueError(f"on_fail must be 'shell' or 'exit', not {on_fail!r}")
    lines = [
        "#!ipxe",
        "dhcp || goto fail",
        f"chain {reg}/boot.ipxe?mac=${{net0/mac}} || goto fail",
        ":fail",
        f"echo LazyOS registry {reg} unreachable",
    ]
    if on_fail == "exit":
        lines += [f"echo booting the local copy in {fail_sleep} s", f"sleep {fail_sleep}", "exit"]
    else:
        lines += ["shell"]
    lines.append("")
    return "\n".join(lines)
