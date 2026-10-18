"""The guest vCPU count as a launcher flag (`run_demo.py --smp`, QEMU `-smp`).

A QEMU argument, so it applies with "Skip build" too. The APs stay parked in
firmware until the kernel's SMP bring-up (docs/smp-plan.md S8.2), so a count
above 1 is safe to ask for and changes nothing until then.
"""

from __future__ import annotations

#: The per-CPU table's capacity the SMP plan fixes (`docs/smp-plan.md` S8.1);
#: the ceiling the CPUs field and `limit.cpu_max` clamp into.
MAX_CPUS = 64


def smp_argv(cfg: dict) -> list[str]:
    """`--smp N` for the Advanced tab's "CPUs" field; empty means QEMU's
    default of 1. Raises ``ValueError`` on a value QEMU cannot take."""
    value = str(cfg.get("smp") or "").strip()
    if not value:
        return []
    if not value.isdigit() or not 1 <= int(value) <= MAX_CPUS:
        raise ValueError(f"CPUs must be a number from 1 to {MAX_CPUS}, not {value!r}")
    return ["--smp", value]
