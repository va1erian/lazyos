"""The netboot payload as an image build switch (`LAZYOS_NETBOOT`, docs/netboot-plan.md N1).

Shared by the launcher (`catalog.py`'s Advanced tab) and mirrored by
`tools/run_demo.py --netboot [--netboot-key PATH]`. The build also writes
`target/netboot/` (the stick's kernel, ramdisk, loader and loader config as
loose files plus `manifest.json`); a key file signs the manifest.
"""

from __future__ import annotations


def netboot_env(cfg: dict) -> dict[str, str]:
    """The environment for the Advanced tab's *Netboot* checkbox and key field.

    The payload is the stick's, so it follows the stick's profile rule: it needs
    `usbd` (``LAZYOS_USB=1``) and a services session, which the desktop implies
    (the build refuses otherwise)."""
    if not cfg.get("netboot"):
        return {}
    env = {"LAZYOS_NETBOOT": "1", "LAZYOS_USB": "1", "LAZYOS_SERVICES": "1"}
    key = str(cfg.get("netboot_key", "")).strip()
    if key:
        # Unsigned otherwise: the build says so, and the registry refuses it.
        env["LAZYOS_NETBOOT_KEY"] = key
    return env
