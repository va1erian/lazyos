"""The optional iPXE text of the registry (docs/netboot-plan.md section 5 and decision 1).

`GET /boot.ipxe?mac=..` is the menu of the machine's builds and
`GET /builds/<channel>/<id>.ipxe` fetches one build's loose files and chains
its loader. They are pure functions of a listing, so the tests pin exactly
what an iPXE client runs. The shape is the Q0 spike's (`spike/scripts.py`):
the menu has a row per build, then "local" (exit to the firmware, which boots
its next entry) and an iPXE shell, and a chain that comes back lands in the
shell so the images can still be inspected.

iPXE can only GET, and a GET does not change registry state: booting from
this menu leaves a queued `next` pending and records nothing. The supported
path with the one-shot `next` is the boot OS's picker (`POST .../booting`).
"""

from __future__ import annotations

import re

#: Loose files the loader needs, in the order the plan fetches them
#: (kernel, then boot.json, then ramdisk; `bootx64.efi` is chained last).
FETCH_ORDER = ("kernel-x86_64", "boot.json", "ramdisk")
LOADER_NAME = "bootx64.efi"

_UNSAFE = re.compile(r"[^A-Za-z0-9 ._:/\-\[\]()+,=]")


def registry_url(host: str, port: int) -> str:
    """`http://host:port`, without `:80`."""
    return f"http://{host}" if port == 80 else f"http://{host}:{port}"


def mib(n: int) -> str:
    return f"{n / (1 << 20):.1f} MiB"


def label_for(build: dict, pending: bool = False) -> str:
    """The menu row of a build; limited to characters an iPXE item label cannot trip over."""
    text = f"{build['id']}  {build['channel']}  {mib(build.get('size', 0))}"
    if pending:
        text += "  [next]"
    return _UNSAFE.sub("?", text)


def boot_menu(reg: str, title: str, builds: list[dict], default_id: str | None,
              timeout_ms: int, pending_id: str | None = None) -> str:
    """`/boot.ipxe`: one row per build (`builds`: `{id, channel, size}`, newest first).

    `default_id` is the row chosen when the countdown ends: the machine's
    queued `next`, else the newest build of its channel; with no builds the
    default is the local copy.
    """
    title = _UNSAFE.sub("?", title)
    rows = {f"b{i}": b for i, b in enumerate(builds)}
    default = next((key for key, b in rows.items() if b["id"] == default_id), "local")
    lines = ["#!ipxe", f"set reg {reg}", ":menu", f"menu LazyOS netboot {title}",
             "item --gap -- builds"]
    lines += [f"item {key} {label_for(b, b['id'] == pending_id)}" for key, b in rows.items()]
    lines += ["item --gap",
              "item local  Boot the local copy (exit to the firmware)",
              "item shell  iPXE shell",
              f"choose --default {default} --timeout {timeout_ms} target || goto local",
              "goto ${target}"]
    for key, b in rows.items():
        lines += [f":{key}", f"chain ${{reg}}/builds/{b['channel']}/{b['id']}.ipxe || goto menu",
                  "goto shell"]
    lines += [":shell", "shell", "goto menu", ":local", "exit", ""]
    return "\n".join(lines)


def build_script(reg: str, build: dict) -> str:
    """`/builds/<channel>/<id>.ipxe`: `imgfetch` the loose files, then chain the loader.

    Every URL is absolute, so the script also works typed by hand
    (`chain <reg>/builds/<channel>/<id>.ipxe`) without `${reg}` set. Raises
    `ValueError` when the manifest lacks the loader (nothing to chain).
    """
    files = build["manifest"].get("files", {})
    if LOADER_NAME not in files:
        raise ValueError(f"build {build['id']} has no {LOADER_NAME}")
    base = f"{reg}/files/{build['channel']}/{build['id']}"
    lines = ["#!ipxe"]
    lines += [f"imgfetch --name {name} {base}/{name}" for name in FETCH_ORDER if name in files]
    lines += [f"chain --name {LOADER_NAME} {base}/{LOADER_NAME}", ""]
    return "\n".join(lines)


def autoexec(reg: str, on_fail: str = "exit", fail_sleep: int = 5) -> str:
    """`autoexec.ipxe` for the ESP: DHCP, then the machine's menu; `on_fail` is `shell` or `exit`.

    `exit` is the plan's section 7 file for the box (back to the firmware,
    which boots its next entry); `shell` keeps a failure on screen.
    """
    if on_fail not in ("shell", "exit"):
        raise ValueError(f"on_fail must be 'shell' or 'exit', not {on_fail!r}")
    lines = ["#!ipxe", "dhcp || goto fail",
             f"chain {reg}/boot.ipxe?mac=${{net0/mac}} || goto fail",
             ":fail", f"echo LazyOS registry {reg} unreachable"]
    if on_fail == "exit":
        lines += [f"echo booting the local copy in {fail_sleep} s", f"sleep {fail_sleep}", "exit"]
    else:
        lines += ["shell"]
    lines.append("")
    return "\n".join(lines)
