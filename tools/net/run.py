#!/usr/bin/env python3
"""Boot LazyOS with a virtio-net card, capture the wire, and judge the capture.

The proof that the network driver works is not a log line but the packets: QEMU
runs with `-netdev user` and a `filter-dump` on the netdev, so every frame the
guest's NIC sends or receives lands in a pcap, which `analyze_pcap.py` then
checks (a complete ARP exchange with the gateway, repeated; no frame outside the
14..1514-byte policy; the hostile-input probe's boundary frames). The serial
markers (`NET:NIC:PASS`, `NICCTL:*:PASS`) only tell the harness when the guest
is done; the verdict comes from the capture.

    python tools/net/run.py                      # build, boot, capture, verify
    python tools/net/run.py --no-build           # reuse target/lazyos.img
    python tools/net/run.py --accel none         # force TCG
    python tools/net/run.py --services           # supervised by `init` as _net
    python tools/net/run.py --machine q35 --virtio-disk
    python tools/net/run.py --no-device          # no NIC: the driver must say so and idle
    python tools/net/run.py --poll               # interrupts off: the driver polls
    python tools/net/run.py --netd               # stage N2: netd, DHCP and ping (plus any variant above)

The image must be built with `LAZYOS_NET=1` (`--netd`: `LAZYOS_NETD=1`, which adds
the stack service `netd` and its tools); this script does it unless `--no-build`. Exit status is non-zero on any failure.
"""

from __future__ import annotations

import argparse
import os
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent
sys.path.insert(0, str(ROOT / "tools" / "screenshot"))
sys.path.insert(0, str(Path(__file__).resolve().parent))
from qemu_qmp import Qmp, accel_args, build_qemu_command, find_qemu, free_port  # noqa: E402

import analyze_pcap  # noqa: E402
import hostpeers  # noqa: E402
import pcap  # noqa: E402
import sockets_pcap  # noqa: E402

#: The soak's iteration count (`netdrv`'s `DEMO_CLIENTS`): one ARP exchange each.
SOAK_ITERATIONS = 40
#: ARP exchanges the demo makes with the gateway: the driver's self-test, the
#: `nicctl arp` client, and one per soak iteration.
MIN_ARP_PAIRS = 1 + 1 + SOAK_ITERATIONS

#: Stage N2 (`--netd`): the stack's evidence. `netd demo=1` runs `netctl`, a real
#: `ping`, the hostile-input probe and a soak once DHCP has completed.
SOAK_RENEWALS = SOAK_ITERATIONS // 10
#: DHCP exchanges: the stack's start, `netctl probe=1`'s renewal, and one renewal
#: every tenth soak round.
NETD_MIN_DHCP = 1 + 1 + SOAK_RENEWALS
#: Echo exchanges with the gateway: `ping 10.0.2.2 4`, the probe's two, and one
#: per soak round.
NETD_MIN_PINGS = 4 + 2 + SOAK_ITERATIONS
#: Stage N3: `netd demo=1` goes on to a name lookup, three `nc` clients against
#: the host's echo servers, the socket probe, the socket soak, and a guest
#: listener the harness connects into. `NC:PASS` is printed by each of the
#: three clients and the listener.
SOCK_SOAK_ITERATIONS = 40
#: Connection attempts to the closed port: every eighth soak round, plus the
#: probe's own. Whether the host's user networking answers them with a reset or
#: with silence differs by host, so the resets are reported, never required.
SOCK_REFUSALS = 1 + SOCK_SOAK_ITERATIONS // 8
#: TCP flows to the echo port: two `nc` runs and one per soak round.
SOCK_TCP_FLOWS = 2 + SOCK_SOAK_ITERATIONS
#: UDP echo pairs: one `nc -u` and one per soak round.
SOCK_UDP_PAIRS = 1 + SOCK_SOAK_ITERATIONS
#: Demo clients `netd` runs in all (`DEMO_CLIENTS` in `user/src/bin/netd.rs`).
NETD_DEMO_CLIENTS = 12
#: The bytes the harness sends into the guest's listener.
INBOUND_BYTES = 150_000
NETD_PASS_MARKERS = (
    "NET:NIC:PASS",
    "NETCTL:INFO:PASS",
    "PING:PASS",
    "NETCTL:PROBE:PASS",
    "NETCTL:SOAK:PASS",
    "NETCTL:SOCKPROBE:PASS",
    "NETCTL:SOCKOWNER:PASS",
    "NETCTL:SOCKSOAK:PASS",
)
NETD_FAIL_MARKERS = (
    "NET:NIC:FAIL",
    "NET:IRQ:FAIL",
    "NETCTL:FAIL",
    "PING:FAIL",
    "NC:FAIL",
    "NSLOOKUP:FAIL",
    "NETD:FAIL",
    "NETDRV:NODEV",
)

#: Serial markers: every PASS must appear; any FAIL (or a missing device) ends
#: the wait early.
PASS_MARKERS = (
    "NET:NIC:PASS",
    "NICCTL:INFO:PASS",
    "NICCTL:ARP:PASS",
    "NICCTL:PROBE:PASS",
    "NICCTL:INTRUDER:PASS",
    "NICCTL:SOAK:PASS",
)
FAIL_MARKERS = (
    "NET:NIC:FAIL",
    "NET:IRQ:FAIL",
    "NICCTL:FAIL",
    "NETDRV:NODEV",
)
LOG_PREFIXES = ("NET", "netdrv", "netd", "NICCTL", "NETDRV", "NETD", "NETCTL", "PING", "NC:", "NSLOOKUP", "Looking up")


def build_image(services: bool, poll: bool, netd: bool = False) -> Path:
    env = dict(os.environ, LAZYOS_NET="1")
    if netd:
        env["LAZYOS_NETD"] = "1"
    else:
        env.pop("LAZYOS_NETD", None)
    if services:
        env["LAZYOS_SERVICES"] = "1"
    if poll:
        env["LAZYOS_NET_ARGS"] = "demo=1 irq=poll"
    else:
        env.pop("LAZYOS_NET_ARGS", None)
    label = "LAZYOS_NET=1" + (" LAZYOS_NETD=1" if netd else "") + (" LAZYOS_SERVICES=1" if services else "") + (" LAZYOS_NET_ARGS='demo=1 irq=poll'" if poll else "")
    print(f"building: {label} cargo build", flush=True)
    result = subprocess.run(["cargo", "build"], cwd=ROOT, env=env, capture_output=True, text=True)
    if result.returncode != 0:
        sys.exit(f"cargo build failed:\n{result.stderr[-4000:]}")
    image = ROOT / "target" / "lazyos.img"
    if not image.is_file():
        sys.exit(f"build succeeded but {image} does not exist")
    return image


def wait_for_marker(serial_log: Path, proc: subprocess.Popen, timeout: float, done, fail_markers=FAIL_MARKERS) -> str:
    """Poll the serial log until `done(text)` or a failure marker, or time runs out."""
    deadline = time.time() + timeout
    text = ""
    printed = 0
    while time.time() < deadline:
        if serial_log.is_file():
            text = serial_log.read_text(errors="replace")
            lines = text.splitlines()
            for line in lines[printed:]:
                if line.startswith(LOG_PREFIXES) or "PANIC" in line or "EXCEPTION" in line:
                    print(f"  {line}", flush=True)
            printed = len(lines)
            if done(text) or any(marker in text for marker in fail_markers):
                return text
        if proc.poll() is not None:
            break
        time.sleep(0.25)
    if serial_log.is_file():
        text = serial_log.read_text(errors="replace")
    return text


def stop_qemu(proc: subprocess.Popen, qmp: Qmp | None) -> None:
    """Quit through QMP so the capture file is closed on the way out."""
    if qmp is not None:
        try:
            qmp.execute("quit")
        except Exception:
            pass
        qmp.close()
    try:
        proc.wait(timeout=15)
    except subprocess.TimeoutExpired:
        proc.terminate()
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()


def netd_done(text: str) -> bool:
    """Stage N3's end of the demo: every marker, a lookup that got an answer,
    the three clients and the listener, and all twelve demo clients reaped."""
    return (
        all(marker in text for marker in NETD_PASS_MARKERS)
        and ("NSLOOKUP:PASS" in text or "NSLOOKUP:NXDOMAIN" in text)
        and text.count("NC:PASS") >= 4
        and text.count("NETD:DEMO:EXIT") >= NETD_DEMO_CLIENTS
    )


def qemu_args(args, image: Path, pcap_path: Path, qemu: str, forward_port: int = 0) -> list[str]:
    extra = list(accel_args(args.accel, qemu) or [])
    if args.machine:
        extra += ["-machine", args.machine]
    if args.virtio_disk:
        extra += [
            "-drive", f"if=none,id=d0,format=raw,file={image.resolve().as_posix()}",
            "-device", "virtio-blk-pci,drive=d0,disable-modern=on",
        ]
    if args.no_device:
        extra += ["-nic", "none"]
    else:
        # Commas in a path are doubled for QEMU's option parser.
        path = pcap_path.resolve().as_posix().replace(",", ",,")
        extra += [
            "-netdev", "user,id=n0" + (
                f",hostfwd=tcp:127.0.0.1:{forward_port}-:{hostpeers.GUEST_LISTEN_PORT}" if forward_port else ""
            ),
            "-device", "virtio-net-pci,netdev=n0",
            "-object", f"filter-dump,id=f0,netdev=n0,file={path}",
        ]
    return extra


def guest_address(text: str) -> bytes:
    """The address `netd` announced (`NETD:ADDR 10.0.2.15/24 ...`)."""
    for line in text.splitlines():
        if line.startswith("NETD:ADDR ") and "/" in line:
            return pcap.parse_ip(line.split()[1].split("/")[0])
    return pcap.parse_ip("10.0.2.15")


def judge_sockets(frames, guest_mac: bytes, text: str, tcp_streams, udp_datagrams, probe) -> bool:
    """Stage N3's verdict from the capture, the host servers and the harness's
    own client: the TCP/UDP/DNS checks of `sockets_pcap.py`."""
    guest_ip = guest_address(text)
    gateway = pcap.parse_ip(analyze_pcap.DEFAULT_GATEWAY)
    ok = True

    def report(name: str, detail: str, problems: list[str]) -> None:
        nonlocal ok
        if problems:
            ok = False
            for problem in problems[:10]:
                print(f"NET:PCAP:{name}:FAIL {problem}")
        else:
            print(f"NET:PCAP:{name}:PASS {detail}".rstrip())

    report("CHECKSUMS", "every TCP segment and UDP datagram from the guest", sockets_pcap.check_checksums(frames, guest_mac))
    count, problems = sockets_pcap.check_echo_flows(frames, guest_ip, gateway, hostpeers.TCP_PORT, SOCK_TCP_FLOWS, tcp_streams)
    report("TCP", f"flows={count} streams match the host server, bytes={sum(len(s) for s in tcp_streams)}", problems)
    count, problems = sockets_pcap.check_refused(frames, guest_ip, gateway, 47_999, SOCK_REFUSALS)
    report("REFUSED", f"no connection to the closed port was established, resets={count}", problems)
    count, problems = sockets_pcap.check_udp_echo(frames, guest_ip, gateway, hostpeers.UDP_PORT, SOCK_UDP_PAIRS, udp_datagrams)
    report("UDP", f"echo pairs={count}", problems)
    detail, problems = sockets_pcap.check_dns(frames, guest_ip, "localhost")
    report("DNS", detail, problems)
    if probe is None:
        report("INBOUND", "", ["the harness never connected into the guest"])
    else:
        problems = probe.verdict()
        if not problems:
            count, problems = sockets_pcap.check_inbound_flow(frames, guest_ip, gateway, hostpeers.GUEST_LISTEN_PORT, probe.payload)
        else:
            count = 0
        report("INBOUND", f"bytes={count} sent into the guest and echoed back", problems)
    return ok


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--image", default=str(ROOT / "target" / "lazyos.img"))
    parser.add_argument("--out", default="shots/net", help="output dir (serial.log, net.pcap)")
    parser.add_argument("--qemu", help="path to qemu-system-x86_64")
    parser.add_argument("--accel", default="auto", choices=["auto", "none", "tcg", "whpx", "kvm"])
    parser.add_argument("--memory", default="256M")
    parser.add_argument("--timeout", type=float, default=150.0, help="seconds to wait for the guest")
    parser.add_argument("--machine", help="QEMU machine type, e.g. q35 (default: i440fx)")
    parser.add_argument(
        "--virtio-disk",
        action="store_true",
        help="attach the image as legacy virtio-blk instead of IDE (needed on q35, "
        "which has no IDE controller the kernel drives)",
    )
    parser.add_argument("--no-build", action="store_true", help="skip the cargo build")
    parser.add_argument("--services", action="store_true", help="build with LAZYOS_SERVICES=1 (init supervises netdrv)")
    parser.add_argument("--no-device", action="store_true", help="boot without a NIC: the driver must say so and idle")
    parser.add_argument("--poll", action="store_true", help="interrupts off: build with `irq=poll`, expect polling")
    parser.add_argument("--netd", action="store_true",
                        help="stage N2: build with LAZYOS_NETD=1 and judge DHCP and ping from the capture")
    parser.add_argument("--min-arp-pairs", type=int, default=None)
    args = parser.parse_args(argv)

    out_dir = Path(args.out) if Path(args.out).is_absolute() else ROOT / args.out
    out_dir.mkdir(parents=True, exist_ok=True)
    serial_log = out_dir / "serial.log"
    pcap_path = out_dir / "net.pcap"
    for stale in (serial_log, pcap_path):
        stale.unlink(missing_ok=True)

    image = Path(args.image) if args.no_build else build_image(args.services, args.poll, args.netd)
    if not image.is_file():
        sys.exit(f"image not found: {image}")

    qemu = find_qemu(args.qemu)
    peers = probe = None
    tcp_streams: list[bytes] = []
    udp_datagrams: list[bytes] = []
    forward_port = 0
    if args.netd and not args.no_device:
        try:
            peers = hostpeers.EchoServers()
        except OSError as exc:
            sys.exit(f"cannot start the host echo servers on ports {hostpeers.TCP_PORT}/{hostpeers.UDP_PORT}: {exc}")
        forward_port = free_port()
        probe = hostpeers.InboundProbe(forward_port, hostpeers.pattern(INBOUND_BYTES))
    extra = qemu_args(args, image, pcap_path, qemu, forward_port)
    port = free_port()
    command = build_qemu_command(qemu, None if args.virtio_disk else str(image), port, serial_log, args.memory, extra)
    print(f"launching: {' '.join(command)}", flush=True)
    proc = subprocess.Popen(command, stdout=subprocess.DEVNULL, stderr=subprocess.STDOUT)

    text = ""
    qmp: Qmp | None = None
    pass_markers = NETD_PASS_MARKERS if args.netd else PASS_MARKERS
    fail_markers = NETD_FAIL_MARKERS if args.netd else FAIL_MARKERS
    if args.no_device:
        # `netd` must keep running and waiting for a driver that never comes.
        done = lambda t: "NETDRV:NODEV" in t and (not args.netd or "NETD:NIC:WAIT" in t)  # noqa: E731
        fail_markers = ()
    elif args.netd:
        def done(t: str) -> bool:
            if probe is not None and f"NC:LISTENING port={hostpeers.GUEST_LISTEN_PORT}" in t:
                probe.start()
            return netd_done(t)
    else:
        done = lambda t: all(marker in t for marker in pass_markers)  # noqa: E731
    try:
        qmp = Qmp("127.0.0.1", port, min(30.0, args.timeout))
        text = wait_for_marker(serial_log, proc, args.timeout, done, fail_markers)
        if probe is not None:
            probe.done.wait(timeout=10)
        time.sleep(1.0)  # let the capture see the last frames
    finally:
        stop_qemu(proc, qmp)
        if peers is not None:
            tcp_streams, udp_datagrams = peers.snapshot()
            peers.close()

    if args.no_device:
        ok = "NETDRV:NODEV" in text and "NET:NIC:FAIL" not in text and "NICCTL:FAIL" not in text
        if args.netd:
            ok = ok and "NETD:READY" in text and "NETD:NIC:WAIT" in text and "NETD:FAIL" not in text
        what = "the driver and netd idled cleanly" if args.netd else "the driver idled cleanly"
        print("NET:HARNESS:" + (f"PASS (no device: {what})" if ok else "FAIL"))
        return 0 if ok else 1

    missing = [marker for marker in pass_markers if marker not in text]
    if args.netd:
        if not ("NSLOOKUP:PASS" in text or "NSLOOKUP:NXDOMAIN" in text):
            missing.append("NSLOOKUP:PASS|NSLOOKUP:NXDOMAIN")
        if text.count("NC:PASS") < 4:
            missing.append(f"NC:PASS x4 (saw {text.count('NC:PASS')})")
    # A failure marker fails the run even when every pass marker also appeared
    # (`wait_for_marker` only stops early on one; it does not judge).
    failed = [marker for marker in fail_markers if marker in text]
    if missing or failed:
        for line in text.splitlines():
            if any(marker in line for marker in fail_markers):
                print(line)
        if failed:
            print(f"NET:HARNESS:FAIL the guest reported {', '.join(failed)}")
        else:
            print(f"NET:HARNESS:FAIL the guest never reported {', '.join(missing)}")
        return 1
    # Interrupts: armed lines must have delivered some; `--poll` and an
    # unroutable line are legitimate polling-only runs and are reported.
    if args.poll:
        if "NETDRV:IRQ:POLLING" not in text or "NET:IRQ:PASS" in text:
            print("NET:HARNESS:FAIL --poll, but the driver did not report polling")
            return 1
        print("NET:IRQ:POLLING (forced by --poll)")
    elif "NETDRV:IRQ:POLLING" in text:
        print("NET:IRQ:POLLING (line not routable on this machine)")
    elif "NET:IRQ:PASS" not in text:
        print("NET:HARNESS:FAIL the interrupt line was armed but the driver saw no interrupt")
        return 1
    else:
        print(next(line for line in text.splitlines() if line.startswith("NET:IRQ:PASS")))
    if args.services and "NETDRV:CRED uid=902 caps=0x100" not in text:
        print("NET:HARNESS:FAIL netdrv did not run as _net (uid 902) with only CAP_DEV_CLAIM")
        return 1
    if args.netd and args.services and "NETD:CRED uid=903 caps=0x0" not in text:
        print("NET:HARNESS:FAIL netd did not run as _netd (uid 903) with no capabilities")
        return 1
    print("NET:GUEST:PASS")

    if not pcap_path.is_file():
        print("NET:HARNESS:FAIL QEMU wrote no capture")
        return 1
    try:
        frames = pcap.read_pcap(pcap_path)
    except pcap.PcapError as exc:
        print(f"NET:PCAP:FAIL {exc}")
        print("NET:HARNESS:FAIL")
        return 1
    mac = next((m for m in (line.split("mac=")[1].split()[0] for line in text.splitlines() if line.startswith("NETDRV:CARD") and "mac=" in line)), None)
    if args.netd:
        # The driver's own ARP self-test is the one exchange that does not come
        # from the stack (whose ARP traffic is whatever its neighbours need).
        arp_pairs = 1 if args.min_arp_pairs is None else args.min_arp_pairs
        extra = dict(expect_probe=False, min_dhcp=NETD_MIN_DHCP, min_pings=NETD_MIN_PINGS,
                     min_frames=2 * (NETD_MIN_DHCP + NETD_MIN_PINGS))
    else:
        arp_pairs = MIN_ARP_PAIRS if args.min_arp_pairs is None else args.min_arp_pairs
        extra = dict(expect_probe=True, min_frames=2 * arp_pairs)
    report = analyze_pcap.analyze(
        frames,
        guest_mac=pcap.parse_mac(mac or analyze_pcap.DEFAULT_GUEST_MAC),
        gateway_ip=pcap.parse_ip(analyze_pcap.DEFAULT_GATEWAY),
        min_arp_pairs=arp_pairs,
        **extra,
    )
    ok = report.ok
    print("\n".join(report.lines))
    if args.netd:
        ok = judge_sockets(frames, pcap.parse_mac(mac or analyze_pcap.DEFAULT_GUEST_MAC), text,
                           tcp_streams, udp_datagrams, probe) and ok
    print("NET:HARNESS:" + ("PASS" if ok else "FAIL"))
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
