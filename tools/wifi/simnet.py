"""The simulated Wi-Fi for interactive boots (`run_demo.py --wifi-sim`, the
launcher's Wi-Fi simulator switch): the build variables, the fake access
points as a background process, and the guest's air card.

The air card is a virtio-net card with the MAC `netpolicy::SIM_AIR_MAC`
(02:57:53:49:4d:00) on a QEMU UDP socket backend whose other end is
`fake_ap.py`; the guest's `netdrv` serves it as `air0` and `wifisim` turns it
into `wlan0` (docs/wifi-prerequisites-plan.md section 3.6).
"""

from __future__ import annotations

import socket
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
AIR_MAC = "02:57:53:49:4d:00"


def build_env(probe: str | None = None) -> dict[str, str]:
    """The image's switches: the stack (without the harness's evidence
    clients), the simulator, and `wifiprobe` started with `probe` if given
    (`open:LazyOpen`, `wpa2:LazyWPA2:lazy-wpa2-pass`, `scan`, `suite:...`)."""
    env = {"LAZYOS_NETD": "1", "LAZYOS_WIFI_SIM": "1"}
    if probe:
        env["LAZYOS_WIFI_SIM_ARGS"] = f"probe={probe}"
    return env


def free_udp_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as s:
        s.bind(("127.0.0.1", 0))
        return int(s.getsockname()[1])


def qemu_args(ap_port: int, guest_port: int) -> list[str]:
    """The air card: one UDP datagram per Ethernet frame, to and from the AP."""
    return ["-netdev", f"socket,id=air,udp=127.0.0.1:{ap_port},localaddr=127.0.0.1:{guest_port}",
            "-device", f"virtio-net-pci,netdev=air,mac={AIR_MAC}"]


def start(log: Path | None = None, pcap: Path | None = None) -> tuple[subprocess.Popen, list[str]]:
    """Start `fake_ap.py` and return it with the QEMU arguments of the air card."""
    ap_port, guest_port = free_udp_port(), free_udp_port()
    command = [sys.executable, str(HERE / "fake_ap.py"), "--ap-port", str(ap_port),
               "--guest-port", str(guest_port)]
    if log:
        command += ["--log", str(log)]
    if pcap:
        command += ["--pcap", str(pcap)]
    process = subprocess.Popen(command, stdout=subprocess.DEVNULL)
    return process, qemu_args(ap_port, guest_port)


def stop(process: subprocess.Popen | None) -> None:
    if process is None:
        return
    process.terminate()
    try:
        process.wait(timeout=5)
    except subprocess.TimeoutExpired:
        process.kill()


def describe(log: Path | None) -> str:
    where = f"; its log: {log}" if log else ""
    return ("Wi-Fi simulator: fake access points LazyOpen (open, ch 1), LazyWPA2 (WPA2, ch 6, "
            "passphrase lazy-wpa2-pass), LazyHidden (hidden, ch 11), LazyRoam (ch 1 and 11), "
            f"LazyWrong; the guest radio is wlan0{where}")
