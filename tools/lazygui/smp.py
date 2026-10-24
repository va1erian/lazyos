"""The guest vCPU count as a launcher flag (`run_demo.py --smp`, QEMU `-smp`),
and the SMP kernel as a build switch (`LAZYOS_SMP=1`, `run_demo.py
--smp-kernel`).

The count is a QEMU argument, so it applies with "Skip build" too. Only an
SMP kernel runs tasks on the APs (docs/smp-plan.md S8.3); any other brings
them up and leaves them idle, so a count above 1 is always safe to ask for.
"""

from __future__ import annotations

#: The per-CPU table's capacity the SMP plan fixes (`docs/smp-plan.md` S8.1);
#: the ceiling the CPUs field and `limit.cpu_max` clamp into.
MAX_CPUS = 64


def smp_kernel_env(cfg: dict) -> dict[str, str]:
    """`LAZYOS_SMP=1` for the Advanced tab's "SMP kernel" box: the image
    build's switch (the kernel entry lock; every CPU runs tasks)."""
    return {"LAZYOS_SMP": "1"} if cfg.get("smp_kernel") else {}


def smp_kernel_argv(cfg: dict) -> list[str]:
    """`--smp-kernel` for `run_demo.py`, which sets `LAZYOS_SMP` itself (and
    clears an inherited one without the flag)."""
    return ["--smp-kernel"] if cfg.get("smp_kernel") else []


def smp_argv(cfg: dict) -> list[str]:
    """`--smp N` for the Advanced tab's "CPUs" field; empty means QEMU's
    default of 1. Raises ``ValueError`` on a value QEMU cannot take."""
    value = str(cfg.get("smp") or "").strip()
    if not value:
        return []
    if not value.isdigit() or not 1 <= int(value) <= MAX_CPUS:
        raise ValueError(f"CPUs must be a number from 1 to {MAX_CPUS}, not {value!r}")
    return ["--smp", value]
