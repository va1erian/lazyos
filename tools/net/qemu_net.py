"""QEMU networking for interactive and scripted boots (stdlib only).

One place that turns "give the guest a network" into QEMU arguments, shared
by `tools/run_demo.py`, the launcher GUI (through run_demo's flags) and the
screenshot tools (`qemu_session.py`, `qemu_shot.py`). The guest gets a
virtio-net card on QEMU's user-mode network ("slirp"):

    guest 10.0.2.15/24 (DHCP)   gateway 10.0.2.2 = the host's loopback
    DNS 10.0.2.3 (the host's resolver)   outbound TCP/UDP NATed by QEMU

Nothing on the host can reach the guest unless a port is forwarded. A forward
is ``[tcp:|udp:][HOSTADDR:]HOSTPORT:GUESTPORT``; the host address defaults to
127.0.0.1, so a forwarded port is reachable from this machine only (name
``0.0.0.0`` explicitly to expose it on the LAN). By default host port 8080
goes to guest port 8080, where the Net Tools app serves a page. See
`docs/networking-host-access.md`.
"""

from __future__ import annotations

import argparse
import ipaddress
import socket
from dataclasses import dataclass

#: The Net Tools web server (xui-app/src/bin/nettools.rs SERVER_PORT).
NETTOOLS_PORT = 8080
#: What `--net` forwards when no `--net-forward` is given.
DEFAULT_FORWARDS = (f"tcp:127.0.0.1:{NETTOOLS_PORT}:{NETTOOLS_PORT}",)
#: QEMU's user network, as the guest sees it.
GUEST_ADDR = "10.0.2.15"
GATEWAY = "10.0.2.2"
DNS = "10.0.2.3"
#: The netdev id (`filter-dump` and `-device` refer to it).
NETDEV_ID = "n0"


@dataclass(frozen=True)
class Forward:
    proto: str
    host_addr: str
    host_port: int
    guest_port: int

    def hostfwd(self) -> str:
        """QEMU's ``hostfwd=`` value."""
        return f"{self.proto}:{self.host_addr}:{self.host_port}-:{self.guest_port}"

    def __str__(self) -> str:
        return f"{self.proto} {self.host_addr}:{self.host_port} -> guest {GUEST_ADDR}:{self.guest_port}"


def _port(text: str, spec: str) -> int:
    if not text.isdigit() or not 1 <= int(text) <= 65535:
        raise ValueError(f"bad port {text!r} in forward {spec!r} (1-65535)")
    return int(text)


def parse_forward(spec: str) -> Forward:
    """Parse ``[tcp:|udp:][HOSTADDR:]HOSTPORT:GUESTPORT``."""
    parts = spec.strip().split(":")
    proto = "tcp"
    if parts and parts[0].lower() in ("tcp", "udp"):
        proto = parts.pop(0).lower()
    if len(parts) == 2:
        host_addr = "127.0.0.1"
    elif len(parts) == 3:
        host_addr = parts.pop(0)
        try:
            ipaddress.IPv4Address(host_addr)
        except ValueError:
            raise ValueError(f"bad host address {host_addr!r} in forward {spec!r}") from None
    else:
        raise ValueError(f"bad forward {spec!r}: use [tcp:|udp:][HOSTADDR:]HOSTPORT:GUESTPORT")
    return Forward(proto, host_addr, _port(parts[0], spec), _port(parts[1], spec))


def forwards_from(specs: list[str] | None) -> list[Forward]:
    """The forwards for a list of ``--net-forward`` values: the defaults when
    none were given, nothing for ``none``. Duplicate host ports are refused
    (QEMU would fail to start with a less helpful message)."""
    if not specs:
        specs = list(DEFAULT_FORWARDS)
    if any(spec.strip().lower() == "none" for spec in specs):
        if len(specs) > 1:
            raise ValueError("--net-forward none cannot be combined with other forwards")
        return []
    forwards = [parse_forward(spec) for spec in specs]
    seen: set[tuple[str, int]] = set()
    for forward in forwards:
        key = (forward.proto, forward.host_port)
        if key in seen:
            raise ValueError(f"host {forward.proto} port {forward.host_port} is forwarded twice")
        seen.add(key)
    return forwards


def netdev_args(forwards: list[Forward], restrict: bool = False,
                pcap: str | None = None) -> list[str]:
    """``-netdev user`` with the forwards, the virtio-net card, and an
    optional packet capture of everything the card sends and receives."""
    netdev = f"user,id={NETDEV_ID}"
    if restrict:
        # The guest reaches nothing outside; forwards still come in.
        netdev += ",restrict=on"
    netdev += "".join(f",hostfwd={forward.hostfwd()}" for forward in forwards)
    args = ["-netdev", netdev, "-device", f"virtio-net-pci,netdev={NETDEV_ID}"]
    if pcap:
        args += ["-object", f"filter-dump,id=netdump,netdev={NETDEV_ID},file={pcap}"]
    return args


def busy_ports(forwards: list[Forward]) -> list[str]:
    """The forwards whose host port something on this machine already holds
    (QEMU would refuse to start with "Could not set up host forwarding")."""
    busy = []
    for forward in forwards:
        kind = socket.SOCK_STREAM if forward.proto == "tcp" else socket.SOCK_DGRAM
        with socket.socket(socket.AF_INET, kind) as probe:
            try:
                probe.bind((forward.host_addr, forward.host_port))
            except OSError:
                busy.append(f"{forward.proto}/{forward.host_addr}:{forward.host_port}")
    return busy


def describe(forwards: list[Forward], restrict: bool = False) -> str:
    """A few lines telling the user how to reach the guest."""
    lines = [f"network: guest {GUEST_ADDR}/24 by DHCP, gateway {GATEWAY} (this host), "
             f"DNS {DNS}" + ("; outbound blocked (--net-restrict)" if restrict else "")]
    for forward in forwards:
        lines.append(f"  forward {forward}")
        if forward.proto == "tcp" and forward.guest_port == NETTOOLS_PORT:
            host = "localhost" if forward.host_addr in ("127.0.0.1", "0.0.0.0") else forward.host_addr
            lines.append(f"    open http://{host}:{forward.host_port} once Net Tools is running")
    if not forwards:
        lines.append("  no ports forwarded: the host cannot connect in (--net-forward)")
    return "\n".join(lines)


def add_net_options(parser: argparse.ArgumentParser, net_help: str) -> None:
    """``--net``, ``--net-forward``, ``--net-restrict`` and ``--net-pcap``."""
    parser.add_argument("--net", action="store_true", help=net_help)
    parser.add_argument("--net-forward", action="append", metavar="SPEC",
                        help="with --net: forward a host port to the guest, "
                             "[tcp:|udp:][HOSTADDR:]HOSTPORT:GUESTPORT (repeatable; HOSTADDR "
                             "defaults to 127.0.0.1). Default: "
                             f"{', '.join(DEFAULT_FORWARDS)} (the Net Tools web server); "
                             "`none` forwards nothing")
    parser.add_argument("--net-restrict", action="store_true",
                        help="with --net: isolate the guest (QEMU restrict=on): no outbound "
                             "traffic, forwarded ports still reach it")
    parser.add_argument("--net-pcap", metavar="PATH",
                        help="with --net: record the card's traffic to PATH (pcap; open it "
                             "with Wireshark or tools/net/analyze_pcap.py)")


def args_from_options(args: argparse.Namespace) -> tuple[list[str], list[Forward]]:
    """The QEMU arguments and forwards ``add_net_options`` asked for (none
    without ``--net``). Raises ValueError for a bad forward."""
    if not args.net:
        if args.net_forward or args.net_restrict or args.net_pcap:
            raise ValueError("--net-forward/--net-restrict/--net-pcap need --net")
        return [], []
    forwards = forwards_from(args.net_forward)
    return netdev_args(forwards, args.net_restrict, args.net_pcap), forwards
