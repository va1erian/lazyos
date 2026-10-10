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
   never restarted), the card is detached and traffic still leaves by `eth0`.
   The guest has no PCI hot-plug handler, so QEMU's eject request is not
   honoured today; the harness then notes it (and still checks that nothing
   else broke) instead of failing.

Frames are located in time with two calibration pings (each card's own
gateway is on-link), because QEMU stamps captures with a clock that is not
always the host's.

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
#: Seconds to wait for a removed card to be detached.
UNPLUG_WAIT = 12.0


def mac_of(card: int) -> str:
    return f"52:54:00:12:34:{0x56 + card:02x}"


def net_of(card: int) -> str:
    """The user network of card `card` (QEMU's gateway is .2, DNS .3)."""
    return f"10.0.{2 + card}"


def build_image(irq_path: str) -> Path:
    # A console image (LAZYOS_CLI, with the BusyBox shell the harness types
    # into; LAZYOS_BUSYBOX names one), supervised by `init`/`devd`.
    env = dict(os.environ, LAZYOS_CLI="1", LAZYOS_NET="1", LAZYOS_NETD="1", LAZYOS_SERVICES="1",
               LAZYOS_NETD_ARGS="demo=0", LAZYOS_NET_ARGS="selftest=0", **irqpath.build_env(irq_path))
    env.pop("LAZYOS_DESKTOP", None)
    print("building: LAZYOS_CLI=1 LAZYOS_NET=1 LAZYOS_NETD=1 LAZYOS_SERVICES=1 LAZYOS_NETD_ARGS=demo=0 cargo build",
          flush=True)
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

    def wait(self, needle: str, timeout: float, after: int = 0, soft: bool = False) -> int:
        """The offset of `needle` in the log past `after`; exits on failure
        (`soft`: -1 when it does not come in time)."""
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
        if soft:
            return -1
        fail(f"timed out waiting for {needle!r}")


def fail(why: str):
    print(f"NET:MULTI:FAIL {why}")
    print("NET:HARNESS:FAIL")
    sys.exit(1)


def type_line(qmp: Qmp, line: str) -> None:
    qmp.type_text(line)
    qmp.press_key("ret")


def wait_for_shell(qmp: Qmp, serial: Serial) -> None:
    """Log in at the console (the development account `user`, password
    `lazy`) and type a marker command until the shell answers: input sent
    before it is up is lost."""
    for _ in range(4):
        if serial.wait("LazyOS login:", 120, soft=True) < 0:
            continue
        type_line(qmp, "user")
        time.sleep(1.0)
        type_line(qmp, "lazy")
        for _ in range(8):
            mark = len(serial.text())
            type_line(qmp, "echo MR:RE''ADY")
            if serial.wait("MR:READY", 5, mark, soft=True) >= 0:
                return
    fail("the console shell never answered")


def run_traffic(qmp: Qmp, serial: Serial, tag: str) -> tuple[float, float]:
    """Ping the off-link address and look a name up, then wait for the shell;
    the host times when Enter was pressed and after the shell answered. The
    marker is built so the echoed command line does not contain it."""
    start = len(serial.text())
    type_line(qmp, f"ping {OFF_LINK} 2; nslookup localhost; echo MR:{tag}:DO''NE")
    began = time.time()  # Enter was just pressed: the guest starts now
    serial.wait(f"MR:{tag}:DONE", 60, start)
    return began, time.time()


def await_link(serial: Serial, before: int, line: str, resolvers: str) -> None:
    """The stack saw the link change and moved the resolvers."""
    serial.wait(line, LINK_WAIT, before)
    serial.wait(f"nameservers={resolvers}", 20, before)


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
    t: dict[str, float] = {}  # host times of the phases
    try:
        qmp = Qmp("127.0.0.1", port, 30.0)
        serial = Serial(serial_log, proc)
        # 1. Both cards get an address (each on its own network).
        serial.wait("NETD:ADDR 10.0.2.15/24", args.timeout)
        serial.wait("NETD:ADDR 10.0.3.15/24", args.timeout)
        wait_for_shell(qmp, serial)
        start = len(serial.text())
        type_line(qmp, "netctl if; echo MR:IF:DO''NE")
        serial.wait("MR:IF:DONE", 60, start)
        listing = serial.text()[start:]
        # Each card's own gateway is on-link: one ping each gives the
        # captures' clocks (see `multi_judge.clock_offset`).
        start = len(serial.text())
        type_line(qmp, "ping 10.0.2.2 1; ping 10.0.3.2 1; echo MR:CAL:DO''NE")
        t["calibrate"] = time.time()  # Enter was just pressed: the guest starts now
        serial.wait("MR:CAL:DONE", 60, start)

        # 2. The cable (metric 100) carries everything.
        t["steady"], t["steady_end"] = run_traffic(qmp, serial, "B")

        # 3. eth0's link goes down: eth1 takes over.
        before = len(serial.text())
        qmp.execute("set_link", name="nic0", up=False)
        await_link(serial, before, "NETD:LINK if=eth0 up=false", "10.0.3.3")
        t["down"], t["down_end"] = run_traffic(qmp, serial, "C")
        down_text = serial.text()[before:]

        # 4. The link comes back: DHCP starts over and eth0 wins again.
        before = len(serial.text())
        t["up_set"] = time.time()
        qmp.execute("set_link", name="nic0", up=True)
        await_link(serial, before, "NETD:LINK if=eth0 up=true", "10.0.2.3")
        serial.wait("NETD:ADDR 10.0.2.15/24", 30, before)
        t["up"], t["up_end"] = run_traffic(qmp, serial, "D")

        # 5. eth1 is hot-unplugged.
        before = len(serial.text())
        qmp.execute("device_del", id="nic1")
        for _ in range(int(UNPLUG_WAIT * 4)):
            if "NETD:NIC:GONE if=eth1" in serial.text()[before:]:
                break
            time.sleep(0.25)
        gone = "NETD:NIC:GONE if=eth1" in serial.text()
        t["unplug"], t["unplug_end"] = run_traffic(qmp, serial, "E")
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
    # The captures' clock against the host's, from the calibration pings.
    offsets = [mj.clock_offset(frames, mac, f"{net_of(c)}.2", t["calibrate"])
               for c, (frames, mac) in enumerate(cards.values())]
    if None in offsets or abs(offsets[0] - offsets[1]) > 3.0:
        fail(f"the captures' clocks cannot be related to the host's (offsets {offsets})")
    offset = sum(offsets) / len(offsets)
    cap = {name: value + offset for name, value in t.items()}  # phase times on the captures' clock
    print("NET:MULTI:CLOCK capture-minus-host offset " + " / ".join(f"{o:.1f}" for o in offsets) + " s; phases at "
          + " ".join(f"{k}={v - t['calibrate']:.1f}" for k, v in t.items()))
    resolver = {f"eth{c}": f"{net_of(c)}.3" for c in range(CARDS)}
    problems: list[str] = []
    note = problems.append

    # Phase 1: DHCP on both cards, before the calibration pings.
    for name, (frames, mac) in cards.items():
        problems += mj.phase_dhcp(f"boot/{name}", frames, mac, 0.0, cap["calibrate"])
    # The listing shows both cards with eth0 primary.
    if "eth0:" not in listing or "eth1:" not in listing:
        note(f"netctl if did not list both cards: {listing!r}")
    elif not any(line.startswith("eth0:") and "primary" in line for line in listing.splitlines()):
        note("eth0 (wired, metric 100) is not marked primary")
    # Phase 2: eth0 carries everything.
    problems += mj.phase_traffic("steady", cards, "eth0", target=OFF_LINK, resolver=resolver,
                                 since=cap["steady"], until=cap["steady_end"])
    # Phase 3: eth1 carries it while eth0's link is down; the lease is kept.
    problems += mj.phase_traffic("link down", cards, "eth1", target=OFF_LINK, resolver=resolver,
                                 since=cap["down"], until=cap["down_end"])
    if "NETD:ADDR none if=eth0" in down_text:
        note("eth0 dropped its address while its link was down (the lease should be kept)")
    # Phase 4: eth0 starts DHCP over and wins again.
    # (A second either side: the clocks are related to within the typing time.)
    problems += mj.phase_dhcp("link up/eth0", cards["eth0"][0], cards["eth0"][1], cap["up_set"] - 1.0,
                              cap["up"] + 1.0, restarted=True)
    problems += mj.phase_traffic("link up", cards, "eth0", target=OFF_LINK, resolver=resolver,
                                 since=cap["up"], until=cap["up_end"])
    # Phase 5: a removed card is detached and the stack never restarted.
    if gone:
        problems += mj.phase_traffic("after unplug", cards, "eth0", target=OFF_LINK, resolver=resolver,
                                     since=cap["unplug"], until=cap["unplug_end"])
        if "eth1:" in after_listing:
            note("netctl still lists eth1 after the card was removed")
    else:
        # Not a failure of the stack: the guest has no PCI hot-plug handler
        # (no ACPI interpreter), so QEMU's eject request is never honoured and
        # the card stays. The detach path is covered by `cargo test -p
        # netstack` (interfaces removed under open sockets) and awaits a
        # guest that completes the eject.
        print("NET:MULTI:NOTE device_del was not honoured: the guest has no PCI hot-plug, the card stayed")
        if "eth1:" not in after_listing:
            note("eth1 vanished from netctl without NETD:NIC:GONE")
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
          "DHCP restarts on link up" + ("; the removed card is detached" if gone else ""))
    print("NET:HARNESS:PASS")
    return 0


if __name__ == "__main__":
    sys.exit(main())
