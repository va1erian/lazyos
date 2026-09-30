#!/usr/bin/env python3
"""Write the small, checked-in seed set for the cargo-fuzz targets.

The seeds are hand-shaped scripts in each target's byte grammar (see
`libs/framering/src/fuzz.rs` and `libs/virtio-net/src/fuzz.rs`), so libFuzzer
starts from inputs that already reach the interesting paths - a full ring, an
index wrap, every kind of scribble, a good and a hostile completion - instead
of rediscovering the grammar from noise.

    python fuzz/gen_corpus.py            # rewrite fuzz/seeds/
    python fuzz/gen_corpus.py --check    # fail if the checked-in seeds differ or extra files appear

Everything here is deterministic; the files are plain bytes.
"""
import argparse
import struct
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent / "seeds"

# ---- framering script grammar -------------------------------------------------

PUSH, POP, ARM, NOTIFY, QUERY, SCRIBBLE, RECREATE = 0, 110, 190, 200, 215, 235, 245


def push(length):
    return bytes([PUSH]) + struct.pack(">H", length)


def scribble(kind, value=0, slot=0):
    return bytes([SCRIBBLE, kind]) + struct.pack(">IH", value, slot)


def first(slots_pick, start_pick):
    """First script byte: slot count 16 << slots_pick (0-4), start index choice."""
    return bytes([start_pick * 5 + slots_pick])


def framering_seeds():
    pop = bytes([POP])
    seeds = {
        # 20 frames into a 16-slot ring, then drain: exercises Full and order.
        "fill_and_drain": first(0, 0) + b"".join(push(60) for _ in range(20)) + pop * 20,
        # The same across the u32 wrap (start index u32::MAX).
        "wrap_u32": first(0, 2) + b"".join(push(14 + i) + pop for i in range(40)),
        # Boundary lengths: 1, 14, 1514, 2046 (max), 2047 (too long), 0 (empty).
        "boundaries": first(1, 0) + b"".join(push(n) for n in (1, 14, 1514, 2046, 2047, 0, 2105)) + pop * 8,
        # Arm / notify coalescing.
        "arm_notify": first(0, 0)
        + bytes([NOTIFY, ARM])
        + push(60) * 3
        + bytes([NOTIFY, NOTIFY, ARM])
        + push(60)
        + bytes([NOTIFY, QUERY])
        + pop * 5,
        # One of every scribble kind, each followed by traffic.
        "scribbles": first(2, 0)
        + b"".join(
            push(100) + scribble(k, 0xDEADBEEF, 3) + pop * 2 + push(60) + pop
            for k in range(7)
        ),
        # A hostile head, then a hostile tail: poisoning must be sticky.
        "poison": first(0, 0) + push(60) + scribble(1, 0x7FFFFFFF) + pop * 3 + push(60) + scribble(2, 99) + push(60),
        # Restarts with different geometries.
        "recreate": b"".join(
            first(i % 5, i % 6) + push(60) + pop + bytes([RECREATE]) + first(i % 5, (i + 1) % 6) for i in range(7)
        ),
        "empty": b"",
    }
    return seeds


def header_seeds():
    return {
        "good_16": bytes([2, 1, 3]),  # 16 slots, exact length, no overwrites
        "bad_len": bytes([2, 2, 0, 0x00, 0x00, 0x41]),
        "bad_slots": bytes([0, 1, 3]),
        "magic_smashed": bytes([2, 1, 3, 0, 0, 0x41, 0, 1, 0x41]),
        "head_nonzero": bytes([2, 1, 3, 0, 0x40, 1]),
        "armed_two": bytes([2, 1, 3, 0, 0xC0, 2]),
        "empty": b"",
    }


# ---- virtio-net grammar -------------------------------------------------------

MAC, STATUS, MQ, MTU = 1 << 5, 1 << 16, 1 << 22, 1 << 3


def virtio_net_seeds():
    cfg_image = bytes([0x52, 0x54, 0x00, 0x12, 0x34, 0x56, 1, 0, 4, 0, 0xDC, 5])
    seeds = {
        "config_mac_status": bytes([0]) + struct.pack("<Q", MAC | STATUS) + cfg_image,
        "config_everything": bytes([0]) + struct.pack("<Q", 0xFFFFFFFFFFFFFFFF) + cfg_image,
        "config_mq_mtu_short": bytes([0]) + struct.pack("<Q", MQ | MTU | MAC) + cfg_image[:7],
        "config_no_features": bytes([0]) + struct.pack("<Q", 0),
    }

    def rx(written, frame_len, flags=0, gso=0):
        hdr = bytes([flags, gso, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0])
        buf = hdr + bytes(range(256)) * 8
        return bytes([1]) + struct.pack("<Q", written) + bytes([0xEA]) + buf[:2048]

    seeds["rx_good_60"] = rx(12 + 60, 60)
    seeds["rx_exact_mtu"] = rx(12 + 1514, 1514)
    seeds["rx_one_over"] = rx(12 + 1515, 1515)
    seeds["rx_runt"] = rx(12 + 13, 13)
    seeds["rx_header_only"] = rx(12, 0)
    seeds["rx_no_header"] = rx(7, 0)
    seeds["rx_overrun"] = rx(0xFFFFFFFF, 0)
    seeds["rx_offload_flags"] = rx(12 + 60, 60, flags=2)
    seeds["rx_gso"] = rx(12 + 60, 60, gso=1)
    seeds["header_plain"] = bytes([2]) + bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0])
    seeds["header_short"] = bytes([2, 1, 2, 3])
    seeds["settings_defaults"] = bytes([3]) + bytes(8)
    seeds["settings_hostile"] = (
        bytes([3]) + bytes([1, 1, 1, 1, 1]) + b"pollzz" + b"FE:ab:CD:00:00:01"
    )
    seeds["settings_mac"] = bytes([3]) + bytes([0, 0, 0, 0, 0]) + b"02:00:00:00:00:01"
    return seeds


# ---- nicdrv script grammar ----------------------------------------------------


def nic_first(rx_pick, tx_pick):
    """First byte: receive entries and transmit entries, each 2 << pick (2..32)
    via the sizes table [2, 4, 16, 64, 256] in `libs/nicdrv/src/fuzz.rs`."""
    return bytes([rx_pick + 5 * tx_pick])


def deliver(length, dst=0):
    return bytes([0]) + struct.pack(">H", length) + bytes([dst])


PUMP, POP_CLIENT, DEV_TX = bytes([70]), bytes([140]), bytes([170])


def push_tx(selector, extra=0):
    """selector: 0 empty, 1 13, 2 14, 3 1514, 4 1515, 5 random u16."""
    return bytes([100, selector]) + (struct.pack(">H", extra) if selector >= 5 else b"")


ATTACH = bytes([190, 0, 0])  # owner OWNER, 16 slots


def nicdrv_seeds():
    seeds = {
        "traffic": nic_first(3, 3) + ATTACH
        + deliver(60) * 5 + PUMP + POP_CLIENT * 6
        + push_tx(2) + push_tx(3) + push_tx(5, 100) + PUMP + DEV_TX * 4 + PUMP,
        "length_boundaries": nic_first(3, 3) + ATTACH
        + b"".join(deliver(n) for n in (13, 14, 1514, 1515, 0, 1699)) + PUMP + POP_CLIENT * 6
        + b"".join(push_tx(s) for s in range(5)) + PUMP + DEV_TX * 5,
        "receive_filter": nic_first(2, 2) + ATTACH
        + b"".join(deliver(60, dst) for dst in range(4)) + PUMP + POP_CLIENT * 4
        + bytes([205, 0, 0, 2]) + deliver(60, 3) + PUMP + POP_CLIENT
        + bytes([205, 0, 0, 0]) + deliver(60, 0) + PUMP + POP_CLIENT,
        "backpressure": nic_first(1, 0) + ATTACH
        + b"".join(push_tx(2) for _ in range(10)) + PUMP + PUMP + DEV_TX * 2 + PUMP + DEV_TX * 2 + PUMP,
        "wake_ups": nic_first(3, 3) + ATTACH
        + bytes([180]) + deliver(60) * 3 + PUMP + bytes([185]) + push_tx(2) + bytes([185]) + PUMP + bytes([185]),
        "attach_abuse": nic_first(3, 3)
        + bytes([190, 0, 3]) + bytes([190, 0, 4]) + bytes([190, 1, 0]) + ATTACH + ATTACH + bytes([190, 1, 0])
        + bytes([200, 1, 0]) + bytes([200, 0, 1]) + bytes([200, 0, 0]) + bytes([210, 2, 0]) + ATTACH
        + bytes([205, 1, 0, 1]) + bytes([205, 0, 0, 7]),
        "link_flap": nic_first(3, 3) + ATTACH + bytes([215, 1]) + PUMP + bytes([215, 0]) + PUMP + bytes([215, 0]) + PUMP,
        "scribble_each": nic_first(3, 3) + b"".join(
            ATTACH + deliver(60) + PUMP + bytes([220, 0, kind]) + struct.pack(">IH", 0xDEADBEEF, 2)
            + push_tx(2) + deliver(60) + PUMP + POP_CLIENT + bytes([245, 0])
            for kind in range(6)
        ),
        "device_lies": nic_first(3, 3) + ATTACH
        + deliver(60) + PUMP
        + bytes([60, 4]) + bytes([1, 2, 3, 4]) + bytes([2]) + PUMP
        + bytes([230, 0, 0]) + struct.pack(">II", 9999, 60) + PUMP,
        "empty": b"",
    }
    return seeds


# ---- netstack script grammar --------------------------------------------------
# `libs/netstack/src/fuzz.rs`: mode byte, four u16 of seed, then ops. 60..=139
# advance the clock (one delay byte) and let the gateway answer, each answer
# followed by a mutation byte (0..4 intact) and a checksum-fix byte; 140..=159
# ping; 160..=169 renew; 185..=199 a gateway-made frame; 200..=214 bend what the
# gateway offers.


def ns_header(mode=1, seed=0x1122334455667788):
    return bytes([mode]) + struct.pack(">Q", seed)


def ns_step(delay=9, mutations=b"\0\0\0\0\0\0"):
    return bytes([100, delay]) + mutations


def ns_ping(dst_kind=0, length=56, timeout=50):
    return bytes([150, dst_kind]) + struct.pack(">H", length) + bytes([timeout])


def netstack_seeds():
    return {
        # DHCP to completion, then a few pings to the gateway.
        "dhcp_then_ping": ns_header() + ns_step() * 12 + ns_ping() + ns_step() * 6 + ns_ping(length=1400) + ns_step() * 6,
        "static_then_ping": ns_header(mode=0) + ns_step() * 4 + ns_ping() + ns_step() * 6,
        # A renewal in the middle of traffic.
        "renew": ns_header() + ns_step() * 12 + bytes([165]) + ns_step() * 12 + ns_ping() + ns_step() * 4,
        # A gateway that offers a hostile lease: loopback, multicast, /31.
        "bent_leases": ns_header() + b"".join(
            bytes([205]) + bytes(ip) + bytes([0, 1, 10, 0, 2, 2, 1, 10, 0, 2, 3, 0, 16, 2])
            + bytes([165]) + ns_step() * 10
            for ip in ((127, 0, 0, 1), (224, 0, 0, 9), (0, 0, 0, 0), (10, 0, 2, 255))
        ),
        # Damaged answers: truncated, bit flips (with and without checksum repair).
        "damage": ns_header() + ns_step(mutations=bytes([5, 0, 1, 7, 1, 2, 1, 0])) * 10 + ns_ping() + ns_step(mutations=bytes([6, 0, 0, 3, 1, 1, 6, 1])) * 10,
        # Raw frames of arbitrary bytes, including an oversize one.
        "raw_frames": ns_header() + bytes([0, 0, 60, 7]) + bytes(range(60)) + bytes([0, 6, 200, 1]) + bytes(range(200)) * 2 + ns_step() * 4,
        # Pings to invalid, off-link and unanswered addresses, then the cap.
        "ping_abuse": ns_header() + ns_step() * 12 + b"".join(ns_ping(dst_kind=k, length=n, timeout=t) for k, n, t in ((1, 8, 10), (2, 0, 50), (3, 1400, 255), (0, 1500, 1), (2, 8, 255), (2, 8, 255), (2, 8, 255), (2, 8, 255), (2, 8, 255), (2, 8, 255), (2, 8, 255), (2, 8, 255), (2, 8, 255))) + ns_step() * 8,
        # Cancel a ping, flip the gateway's echo behaviour.
        "cancel_and_mute": ns_header() + ns_step() * 12 + ns_ping() + bytes([182, 0, 0]) + bytes([172, 1]) + ns_ping() + ns_step() * 8,
        "empty": b"",
    }


TARGETS = {
    "framering": framering_seeds,
    "framering_header": header_seeds,
    "virtio_net": virtio_net_seeds,
    "nicdrv": nicdrv_seeds,
    "netstack": netstack_seeds,
}


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--check", action="store_true", help="fail if the checked-in corpus differs")
    args = parser.parse_args()
    bad = False
    for target, make in TARGETS.items():
        directory = ROOT / target
        for name, data in make().items():
            path = directory / name
            if args.check:
                if not path.exists() or path.read_bytes() != data:
                    print(f"stale: {path}")
                    bad = True
            else:
                directory.mkdir(parents=True, exist_ok=True)
                path.write_bytes(data)
    if args.check:
        # Anything else in seeds/ is libFuzzer output that leaked in; the
        # working corpus lives in the git-ignored fuzz/corpus/.
        wanted = {ROOT / target / name for target, make in TARGETS.items() for name in make()}
        for path in sorted(ROOT.rglob("*")):
            if path.is_file() and path not in wanted:
                print(f"unexpected: {path}")
                bad = True
        return 1 if bad else 0
    print(f"wrote the seeds under {ROOT}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
