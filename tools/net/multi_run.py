#!/usr/bin/env python3
"""Boot LazyOS with two network cards and judge `netd`'s multi-interface
behaviour from the packet captures (WP1, docs/wifi-prerequisites-plan.md).

    python tools/net/run.py --nics 2          # build, boot, drive, judge
    python tools/net/multi_run.py --no-build  # the same, reusing target/lazyos.img

Two virtio-net cards, each on its own user network (10.0.2.0/24 and
10.0.3.0/24) with its own `filter-dump` capture. The stack starts under
`init`/`devd` (so the cards are named `eth0` and `eth1`) with no demo clients;
the harness types into the console shell over QMP and judges only frames:

1. both cards complete DHCP (DISCOVER, OFFER, REQUEST, ACK on each capture);
2. traffic to an off-link address and a DNS lookup leave by the wired-metric
   winner, `eth0`, and by nothing else;
3. QMP `set_link` takes `eth0` down: the same traffic now leaves by `eth1`;
4. the link comes back: `eth0` starts DHCP over (a new DISCOVER and a complete
   exchange) and wins the traffic again;
5. the second card is hot-unplugged (`device_del`): `netd` stays up (it was
   never restarted) and traffic still leaves by `eth0`.

The serial log only says when a phase can start. See `multi_judge.py` for the
checks and `test_multi_judge.py` for their self-test.
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
sys.path.insert(0, str(ROOT / "tools"))
from qemu_qmp import DEFAULT_MEMORY, Qmp, accel_args, build_qemu_command, find_qemu, free_port  # noqa: E402

import irqpath  # noqa: E402
import multi_judge as mj  # noqa: E402
import pcap  # noqa: E402
from devd_markers import DEVD_FAIL_MARKERS  # noqa: E402
from harness_io import stop_qemu  # noqa: E402

#: An address no network routes: only the default route can carry it.
OFF_LINK = "192.0.2.55"
CARDS = 2
#: Seconds the driver may take to report a link change (it polls about once a second).
LINK_WAIT = 20.0


def mac_of(card: int) -> str:
    return f"52:54:00:12:34:{0x56 + card:02x}"


def net_of(card: int) -> str:
    """The user network of card `card` (QEMU's gateway is .2, DNS .3)."""
    return f"10.0.{2 + card}"


def build_image(irq_path: str) -> Path:
    env = dict(os.environ, LAZYOS_NET="1", LAZYOS_NETD="1", LAZYOS_SERVICES="1",
               LAZYOS_NETD_ARGS="demo=0", LAZYOS_NET_ARGS="selftest=0", **irqpath.build_env(irq_path))
    print("building: LAZYOS_NET=1 LAZYOS_NETD=1 LAZYOS_SERVICES=1 LAZYOS_NETD_ARGS=demo=0 cargo build", flush=True)
    result = subprocess.run(["cargo", "build"], cwd=ROOT, env=env, capture_output=True, text=True)
    if result.returncode != 0:
        sys.exit(f"cargo build failed:\n{result.stderr[-4000:]}")
    return ROOT / "target" / "lazyos.img"


def qemu_cards(out_dir: Path, accel: str, qemu: str) -> list[str]:
    extra = list(accel_args(accel, qemu) or [])
    for card in range(CARDS):
        path = (out_dir / f"net{card}.pcap").resolve().as_posix().replace(",", ",,")
        extra += [
            "-netdev", f"user,id=n{card},net={net_of(card)}.0/24",
            "-device", f"virtio-net-pci,netdev=n{card},mac={mac_of(card)},id=nic{card}",
            "-object", f"filter-dump,id=f{card},netdev=n{card},file={path}",
        ]
    return extra


class Serial:
    """The guest's serial log, read as it grows."""

    def __init__(self, path: Path, proc: subprocess.Popen):
        self.path, self.proc, self.printed = path, proc, 0

    def text(self) -> str:
        text = self.path.read_text(errors="replace") if self.path.is_file() else ""
        lines = text.splitlines()
        for line in lines[self.printed:]:
            if line.startswith(("NET", "DEVD", "INIT:DRIVER", "MR:", "PING", "NSLOOKUP", "eth")):
                print(f"  {line}", flush=True)
        self.printed = len(lines)
        return text

    def wait(self, needle: str, timeout: float, after: int = 0) -> int:
        """The offset of `needle` in the log past `after`; exits on failure."""
        deadline = time.time() + timeout
        while time.time() < deadline:
            text = self.text()
            at = text.find(needle, after)
            if at >= 0:
                return at + len(needle)
            if any(m in text for m in ("NETD:FAIL", "PANIC", *DEVD_FAIL_MARKERS)):
                fail(f"the guest reported a failure while waiting for {needle!r}")
            if self.proc.poll() is not None:
                fail(f"QEMU exited while waiting for {needle!r}")
            time.sleep(0.25)
        fail(f"timed out waiting for {needle!r}")


def fail(why: str):
    print(f"NET:MULTI:FAIL {why}")
    print("NET:HARNESS:FAIL")
    sys.exit(1)


def type_line(qmp: Qmp, line: str) -> None:
    qmp.type_text(line)
    qmp.press_key("ret")


def run_traffic(qmp: Qmp, serial: Serial, tag: str) -> None:
    """Ping the off-link address and look a name up, then wait for the shell.
    The marker is built so the echoed command line does not contain it."""
    start = len(serial.text())
    type_line(qmp, f"ping {OFF_LINK} 2; nslookup localhost; echo MR:{tag}:DO''NE")
    serial.wait(f"MR:{tag}:DONE", 60, start)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--image", default=str(ROOT / "target" / "lazyos.img"))
    parser.add_argument("--out", default="shots/net_multi")
    parser.add_argument("--qemu")
    parser.add_argument("--accel", default="auto", choices=["auto", "none", "tcg", "whpx", "kvm"])
    parser.add_argument("--memory", default=DEFAULT_MEMORY)
    parser.add_argument("--timeout", type=float, default=240.0, help="seconds to boot and get both addresses")
    parser.add_argument("--no-build", action="store_true")
    parser.add_argument("--nics", type=int, default=CARDS, help="only 2 is supported")
    irqpath.add_option(parser)
    args = parser.parse_args(argv)
    if args.nics != CARDS:
        sys.exit(f"--nics {args.nics}: the harness drives exactly {CARDS} cards")

    out_dir = Path(args.out) if Path(args.out).is_absolute() else ROOT / args.out
    out_dir.mkdir(parents=True, exist_ok=True)
    serial_log = out_dir / "serial.log"
    for stale in (serial_log, *(out_dir / f"net{c}.pcap" for c in range(CARDS))):
        stale.unlink(missing_ok=True)
    image = Path(args.image) if args.no_build else build_image(args.irq_path)
    if not image.is_file():
        sys.exit(f"image not found: {image}")

    qemu = find_qemu(args.qemu)
    port = free_port()
    command = build_qemu_command(qemu, str(image), port, serial_log, args.memory, qemu_cards(out_dir, args.accel, qemu))
    print(f"launching: {' '.join(command)}", flush=True)
    proc = subprocess.Popen(command, stdout=subprocess.DEVNULL, stderr=subprocess.STDOUT)
    qmp = None
    marks: dict[str, float] = {}
    try:
        qmp = Qmp("127.0.0.1", port, 30.0)
        serial = Serial(serial_log, proc)
        marks["boot"] = time.time()
        # 1. Both cards get an address (each on its own network).
        serial.wait("NETD:ADDR 10.0.2.15/24", args.timeout)
        serial.wait("NETD:ADDR 10.0.3.15/24", args.timeout)
        serial.wait("/ #", 60)
        marks["leases"] = time.time()
        start = len(serial.text())
        type_line(qmp, "netctl if; echo MR:IF:DO''NE")
        serial.wait("MR:IF:DONE", 60, start)
        listing = serial.text()[start:]

        # 2. The cable (metric 100) carries everything.
        marks["steady"] = time.time()
        run_traffic(qmp, serial, "B")
        marks["steady_end"] = time.time()

        # 3. eth0's link goes down: eth1 takes over.
        before = len(serial.text())
        qmp.execute("set_link", name="nic0", up=False)
        marks["down"] = time.time()
        serial.wait("NETD:LINK if=eth0 up=false", LINK_WAIT, before)
        serial.wait("nameservers=10.0.3.3", 20, before)
        marks["down_seen"] = time.time()
        run_traffic(qmp, serial, "C")
        marks["down_end"] = time.time()
        down_text = serial.text()[before:]

        # 4. The link comes back: DHCP starts over and eth0 wins again.
        before = len(serial.text())
        qmp.execute("set_link", name="nic0", up=True)
        marks["up"] = time.time()
        serial.wait("NETD:LINK if=eth0 up=true", LINK_WAIT, before)
        serial.wait("NETD:ADDR 10.0.2.15/24", 30, before)
        serial.wait("nameservers=10.0.2.3", 20, before)
        marks["up_seen"] = time.time()
        run_traffic(qmp, serial, "D")
        marks["up_end"] = time.time()

        # 5. eth1 is hot-unplugged.
        before = len(serial.text())
        marks["unplug"] = time.time()
        qmp.execute("device_del", id="nic1")
        gone = "NETD:NIC:GONE if=eth1" in serial.text()
        for _ in range(int(LINK_WAIT * 4)):
            if "NETD:NIC:GONE if=eth1" in serial.text()[before:]:
                gone = True
                break
            time.sleep(0.25)
        marks["unplug_seen"] = time.time()
        if gone:
            run_traffic(qmp, serial, "E")
        marks["unplug_end"] = time.time()
        start = len(serial.text())
        type_line(qmp, "netctl if; echo MR:IF2:DO''NE")
        serial.wait("MR:IF2:DONE", 60, start)
        after_listing = serial.text()[start:]
        time.sleep(1.0)  # let the captures see the last frames
    finally:
        stop_qemu(proc, qmp)

    text = serial_log.read_text(errors="replace")
    cards = {}
    for card in range(CARDS):
        try:
            cards[f"eth{card}"] = (pcap.read_pcap(out_dir / f"net{card}.pcap"), pcap.parse_mac(mac_of(card)))
        except pcap.PcapError as exc:
            fail(f"card {card}: {exc}")
    resolver = {f"eth{c}": f"{net_of(c)}.3" for c in range(CARDS)}
    problems: list[str] = []
    note = problems.append

    # Phase 1: DHCP on both cards.
    for name, (frames, mac) in cards.items():
        problems += mj.phase_dhcp(f"boot/{name}", frames, mac, marks["boot"] - 5, marks["leases"] + 5)
    # The listing shows both cards with eth0 primary.
    if "eth0:" not in listing or "eth1:" not in listing:
        note(f"netctl if did not list both cards: {listing!r}")
    elif not any(line.startswith("eth0:") and "primary" in line for line in listing.splitlines()):
        note("eth0 (wired, metric 100) is not marked primary")
    # Phase 2: eth0 carries everything.
    problems += mj.phase_traffic("steady", cards, "eth0", target=OFF_LINK, resolver=resolver,
                                 since=marks["steady"], until=marks["steady_end"])
    # Phase 3: eth1 carries it while eth0's link is down; the lease is kept.
    problems += mj.phase_traffic("link down", cards, "eth1", target=OFF_LINK, resolver=resolver,
                                 since=marks["down_seen"], until=marks["down_end"])
    if "NETD:ADDR none if=eth0" in down_text:
        note("eth0 dropped its address while its link was down (the lease should be kept)")
    # Phase 4: eth0 starts DHCP over and wins again.
    problems += mj.phase_dhcp("link up/eth0", cards["eth0"][0], cards["eth0"][1], marks["up"], marks["up_seen"] + 5,
                              restarted=True)
    problems += mj.phase_traffic("link up", cards, "eth0", target=OFF_LINK, resolver=resolver,
                                 since=marks["up_seen"], until=marks["up_end"])
    # Phase 5: the unplugged card is detached; the stack was never restarted.
    if "NETD:NIC:GONE if=eth1" in text:
        problems += mj.phase_traffic("after unplug", cards, "eth0", target=OFF_LINK, resolver=resolver,
                                     since=marks["unplug_seen"], until=marks["unplug_end"])
        if "eth1:" in after_listing:
            note("netctl still lists eth1 after the card was removed")
    else:
        note("the removed card was never detached (no NETD:NIC:GONE if=eth1)")
    if text.count("NETD:READY") != 1:
        note(f"netd started {text.count('NETD:READY')} times: it must survive all of this")
    if "NETD:FAIL" in text or "PANIC" in text:
        note("the guest reported a failure")

    if problems:
        for problem in problems:
            print(f"NET:MULTI:FAIL {problem}")
        print("NET:HARNESS:FAIL")
        return 1
    print("NET:MULTI:PASS DHCP on both cards; eth0 wins; eth1 takes over on link down; "
          "DHCP restarts on link up; the removed card is detached")
    print("NET:HARNESS:PASS")
    return 0


if __name__ == "__main__":
    sys.exit(main())
