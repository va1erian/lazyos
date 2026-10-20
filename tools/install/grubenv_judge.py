"""Judge the U2a spike (docs/update-plan.md decision 6): GRUB counting LazyOS
trial boots in `/releases/grubenv` on the OS volume.

Pure functions over text and bytes, so ``test_grubenv_judge.py`` shows they
fail when they should without booting anything.
"""

from __future__ import annotations

import re

import judge

HEADER = b"# GRUB Environment Block\n"
BLOCK = 1024
BEGIN = "### BEGIN lazyos-install ###"
END = "### END lazyos-install ###"
CUSTOM_SOURCE = "if [ -f ${config_directory}/custom.cfg ]"

# (counter before the boot, entry GRUB must pick, counter after it).
COUNTDOWN = [("4", "lazyos", "3"), ("3", "lazyos", "2"), ("2", "lazyos", "1"),
             ("1", "lazyos", "0"), ("0", "linux", "0")]
# The /etc/grub.d placement: the last two rungs, then the default.
GRUBD = [("1", "lazyos", "0"), ("0", "linux", "0")]
# A value no rung matches leaves GRUB's own choice and the file as it was.
CORRUPT = [("x", "linux", "x"), ("5", "linux", "5"), ("", "linux", "")]


def env_block(values: dict[str, str]) -> bytes:
    """A GRUB environment block, `#`-padded to 1024 bytes as grub-editenv writes it."""
    body = HEADER + "".join(f"{k}={v}\n" for k, v in values.items()).encode()
    if len(body) > BLOCK:
        raise ValueError("does not fit an environment block")
    return body + b"#" * (BLOCK - len(body))


def counter(block: bytes) -> str | None:
    """`lazyos_boots` in an environment block; None when it is not a valid
    block (length, header) or has no such line."""
    if len(block) != BLOCK or not block.startswith(HEADER):
        return None
    match = re.search(rb"^lazyos_boots=(.*)$", block, re.M)
    return match.group(1).decode() if match else None


def entry_booted(log: str) -> str | None:
    """`lazyos` (the loader printed BOOT:MEDIA), `linux` (the stand-in ran),
    or None when neither (or both: the log is not one boot)."""
    lazyos = bool(judge.boot_judge.MEDIA.search(log))
    linux = judge.LINUX_STANDIN in log
    return "lazyos" if lazyos and not linux else "linux" if linux and not lazyos else None


def judge_boot(log: str, env: bytes, expect_entry: str, expect_counter: str | None) -> list[str]:
    failures = judge.serial_integrity.problems(log)
    booted = entry_booted(log)
    if booted != expect_entry:
        failures.append(f"GRUB booted {booted or 'neither entry (or both)'}, expected {expect_entry}")
    seen = counter(env)
    if seen != expect_counter:
        failures.append(f"lazyos_boots is {seen!r} after the boot, expected {expect_counter!r}")
    if len(env) != BLOCK:
        failures.append(f"grubenv is {len(env)} bytes, not {BLOCK}")
    return failures


def judge_linux_side(before: dict[str, bytes], after: dict[str, bytes]) -> list[str]:
    """The distro's own files (its grub.cfg, GRUB, its grubenv with
    `saved_entry`) are byte-identical: the snippet never touches them."""
    return [f"the Linux side's {name} changed" for name in sorted(before)
            if after.get(name) != before[name]]


def grub_errors(log: str) -> list[str]:
    """GRUB's own `error:` lines, as evidence in the report."""
    return re.findall(r"^error: .*$", log, re.M)


def splice_grubd(grub_cfg: str, custom: str) -> tuple[str, str]:
    """Move the marked block of `custom.cfg` into `grub.cfg` where
    `/etc/grub.d/40_lazyos`'s output would be (before 41_custom's source line):
    the other placement of the same text. Returns (grub.cfg, custom.cfg)."""
    start, end = custom.find(BEGIN), custom.find(END)
    if start < 0 or end < start:
        raise ValueError("no lazyos-install block in custom.cfg")
    end += len(END) + 1
    block = custom[start:end]
    at = grub_cfg.find(CUSTOM_SOURCE)
    if at < 0:
        raise ValueError("grub.cfg does not source custom.cfg")
    return grub_cfg[:at] + block + grub_cfg[at:], custom[:start] + custom[end:]
