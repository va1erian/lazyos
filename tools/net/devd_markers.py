"""What a supervised boot must show of `devd` (issue #497).

With `LAZYOS_SERVICES=1` (and `devd` not switched off with `LAZYOS_DEVD=0`),
`init` starts `devd` instead of the drivers: `devd` matches the device, asks
`init` for the driver row, sees the driver claim it and finds its retained
topics on the broker. The network and sound harnesses add these to their
pass markers.
"""

from __future__ import annotations

import os


def devd_markers(driver: str) -> tuple[str, ...]:
    """The serial markers for a boot where `devd` starts `driver`; none when
    the build switched `devd` off."""
    if os.environ.get("LAZYOS_DEVD") == "0":
        return ()
    return (
        "DEVD:CRED uid=906 caps=0x0",
        f"INIT:DRIVER:START driver={driver} dev=",
        "DEVD:CLAIMED:PASS",
        "DEVD:TOPICS:PASS",
    )


#: A `devd` that could not start its driver or died.
DEVD_FAIL_MARKERS = ("DEVD:FAIL", "DEVD:START:FAIL", "INIT:DRIVER:DENIED")
