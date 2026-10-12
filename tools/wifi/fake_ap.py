#!/usr/bin/env python3
"""Fake Wi-Fi access points for the simulated radio `wifisim`
(docs/wifi-prerequisites-plan.md section 3.6).

QEMU connects the guest's air card (MAC 02:57:53:49:4d:00) to this process:

    -netdev socket,id=air,udp=127.0.0.1:<ap port>,localaddr=127.0.0.1:<guest port>
    -device virtio-net-pci,netdev=air,mac=02:57:53:49:4d:00

One UDP datagram is one Ethernet frame carrying one 802.11 frame
(`fakeap/air.py`, the same format as `libs/simair`). The default scenario:

    LazyOpen    open       channel 1    02:a0:00:00:00:01
    LazyWPA2    WPA2-PSK   channel 6    02:a0:00:00:00:02   passphrase lazy-wpa2-pass
    LazyHidden  open, hidden SSID, channel 11                02:a0:00:00:00:03
    LazyRoam    WPA2-PSK   channels 1 and 11 (two BSSes)     02:a0:00:00:00:04/05
    LazyWrong   WPA2-PSK   channel 6, a passphrase no station has   02:a0:00:00:00:06

Behind every BSS: a DHCP server (192.168.77.100+ /24, gateway .1), ARP for
the gateway, echo replies, and five pings from the gateway once a station
has its lease. Faults on demand, on the control port (one UDP datagram per
command) or scheduled with `--schedule` (seconds after the first station is
authorized): `deauth [reason]`, `silence <seconds>` (beacon loss: the AP
stops sending and answering), `rekey` (group key handshake). A wrong
passphrase needs nothing: join LazyWrong, and the AP refuses message 2
(`AP:EAPOL:M2:BADMIC`).

    python tools/wifi/fake_ap.py --ap-port 47011 --guest-port 47010 --pcap shots/wifi/air.pcap
    python tools/wifi/fake_ap.py --self-test

Everything the AP does is an `AP:` line on stdout (and `--log`); every frame
on the air goes to the pcap (link type 105, 802.11 without radiotap).
"""

from __future__ import annotations

import argparse
import socket
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from fakeap import air, ccmp, wpa  # noqa: E402
from fakeap.ap import World  # noqa: E402
from fakeap.station import Bss  # noqa: E402

WPA2_PASSPHRASE = "lazy-wpa2-pass"
WRONG_PASSPHRASE = "not-the-stations-passphrase"


def scenario(passphrase: str = WPA2_PASSPHRASE) -> list[Bss]:
    def bssid(n: int) -> bytes:
        return bytes([0x02, 0xA0, 0, 0, 0, n])
    return [
        Bss("LazyOpen", 1, bssid(1), rssi=-40),
        Bss("LazyWPA2", 6, bssid(2), "wpa2", passphrase, rssi=-45),
        Bss("LazyHidden", 11, bssid(3), hidden=True, rssi=-55),
        Bss("LazyRoam", 1, bssid(4), "wpa2", passphrase, rssi=-70),
        Bss("LazyRoam", 11, bssid(5), "wpa2", passphrase, rssi=-50),
        # The wrong-passphrase fault: no station knows this one's passphrase.
        Bss("LazyWrong", 6, bssid(6), "wpa2", WRONG_PASSPHRASE, rssi=-60),
    ]


def parse_schedule(text: str | None) -> list[tuple[float, str]]:
    """`rekey:10,deauth 3:20` as (seconds, command), sorted."""
    out = []
    for item in (text or "").split(","):
        if item.strip():
            command, _, at = item.rpartition(":")
            out.append((float(at), command.strip()))
    return sorted(out)


class Logger:
    def __init__(self, path: Path | None):
        self.file = open(path, "a", encoding="utf-8") if path else None

    def __call__(self, line: str) -> None:
        stamped = f"{line}"
        print(stamped, flush=True)
        if self.file:
            self.file.write(stamped + "\n")
            self.file.flush()


def serve(args: argparse.Namespace) -> int:
    log = Logger(args.log)
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.bind(("127.0.0.1", args.ap_port))
    guest = ("127.0.0.1", args.guest_port)
    control = None
    if args.control_port:
        control = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        control.bind(("127.0.0.1", args.control_port))
        control.setblocking(False)
    pcap = air.PcapWriter(args.pcap)

    def send(header: air.Air, frame: bytes) -> None:
        sock.sendto(air.encode(air.AP_CARD_MAC, header, frame), guest)

    world = World(scenario(args.passphrase), send, pcap, log)
    schedule = parse_schedule(args.schedule)
    log(f"AP:START bsses={len(world.bsses)} ap_port={args.ap_port} guest_port={args.guest_port}")
    stop_at = time.monotonic() + args.duration if args.duration else None
    try:
        while stop_at is None or time.monotonic() < stop_at:
            now = time.monotonic()
            due = world.tick(now)
            if schedule and world.first_authorized is not None:
                while schedule and now >= world.first_authorized + schedule[0][0]:
                    world.fault(schedule.pop(0)[1], now)
            if control is not None:
                try:
                    command = control.recv(512).decode(errors="replace").strip()
                    if command == "quit":
                        break
                    world.fault(command, now)
                except (BlockingIOError, ConnectionResetError):
                    pass
            sock.settimeout(max(0.001, min(due - time.monotonic(), 0.05)))
            try:
                data = sock.recv(65536)
            except (socket.timeout, ConnectionResetError):
                # Windows reports an ICMP port-unreachable from a send as a
                # reset on the next receive (QEMU not listening yet).
                continue
            decoded = air.decode(data)
            if decoded is not None:
                world.receive(decoded[0], decoded[1], time.monotonic())
    except KeyboardInterrupt:
        pass
    finally:
        pcap.close()
        log("AP:STOP")
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--ap-port", type=int, default=47011, help="UDP port this AP listens on")
    parser.add_argument("--guest-port", type=int, default=47010, help="UDP port QEMU's air card listens on")
    parser.add_argument("--control-port", type=int, help="UDP port for fault commands")
    parser.add_argument("--pcap", type=Path, help="record every 802.11 frame here")
    parser.add_argument("--log", type=Path, help="append the AP: lines here too")
    parser.add_argument("--passphrase", default=WPA2_PASSPHRASE,
                        help="the WPA2 networks' passphrase (another one than the station's is the "
                             "wrong-passphrase fault)")
    parser.add_argument("--schedule", help="faults after the first authorization, e.g. 'rekey:8,silence 4:20'")
    parser.add_argument("--duration", type=float, help="stop after this many seconds")
    parser.add_argument("--self-test", action="store_true", help="check the crypto against IEEE 802.11 Annex J")
    args = parser.parse_args(argv)
    if args.self_test:
        ccmp.self_test()
        wpa.self_test()
        print("fake_ap: self-test passed")
        return 0
    return serve(args)


if __name__ == "__main__":
    sys.exit(main())
