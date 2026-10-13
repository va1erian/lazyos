"""The BusyBox row's `cd` check of the ABI bench (split out of `run.py`)."""

from __future__ import annotations

import re


def check_busybox_cwd(serial: str) -> tuple[str, str]:
    """`cd /data` must move the kernel's cwd, not just the shell's prompt (#365).

    The script prints `pwd -P` (which asks the kernel via `getcwd`) and `ls`
    (a forked and exec'd applet, so the directory must survive `execve`) after
    `cd /data`, then again after `cd ..`, each under its own marker line.
    """
    # `ls` colours its output because the serial console looks like a terminal.
    text = re.sub(r"\x1b?\[[0-9;]*m", "", serial)

    def section(start: str, end: str) -> str:
        pattern = rf"^ABI:busybox:{start}\r?\n(.*?)^ABI:busybox:{end}"
        found = re.search(pattern, text, re.M | re.S)
        return found.group(1) if found else ""

    if "/data" not in [line.strip() for line in section("CWD", "LS").splitlines()]:
        return "fail", "pwd -P after cd /data is not /data"
    if "cwdprobe" not in section("LS", "CWD2").split():
        return "fail", "ls after cd /data does not list the file echo wrote there"
    if "/" not in [line.strip() for line in section("CWD2", "LS2").splitlines()]:
        return "fail", "pwd -P after cd .. is not /"
    root = section("LS2", "END").split()
    if "cwdprobe" in root or "system" not in root:
        return "fail", "ls after cd .. does not list the boot volume"
    return "pass", ""
