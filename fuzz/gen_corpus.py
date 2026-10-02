#!/usr/bin/env python3
"""Write the small, checked-in seed set for the cargo-fuzz targets.

The seeds are hand-shaped scripts in each target's byte grammar (see
`libs/framering/src/fuzz.rs` and `libs/virtio-net/src/fuzz.rs`), so libFuzzer
starts from inputs that already reach the interesting paths - a full ring, an
index wrap, every kind of scribble, a good and a hostile completion - instead
of rediscovering the grammar from noise. The `lazypkg` target takes real `.lzp`
zip archives instead, built here with `struct`/`zlib` so the bytes are
deterministic across Python versions (Python's `zipfile` output is not).

    python fuzz/gen_corpus.py            # rewrite fuzz/seeds/
    python fuzz/gen_corpus.py --check    # fail if the checked-in seeds differ or extra files appear

Everything here is deterministic; the files are plain bytes.
"""
import argparse
import struct
import sys
import zlib
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


# ---- lazypkg zip grammar ------------------------------------------------------


def _crc32(data):
    return zlib.crc32(data) & 0xFFFFFFFF


def _zip(entries):
    """Serialize `(name, data, deflate)` tuples into a deterministic zip."""
    local = b""
    central = b""
    for name, data, deflate in entries:
        name_bytes = name.encode("utf-8")
        crc = _crc32(data)
        if deflate:
            compressor = zlib.compressobj(9, zlib.DEFLATED, -15)
            stored = compressor.compress(data) + compressor.flush()
            method = 8
        else:
            stored = data
            method = 0
        offset = len(local)
        local += struct.pack(
            "<IHHHHHIIIHH",
            0x04034B50, 20, 0, method, 0, 0, crc, len(stored), len(data), len(name_bytes), 0,
        )
        local += name_bytes + stored
        central += struct.pack(
            "<IHHHHHHIIIHHHHHII",
            0x02014B50, 20, 20, 0, method, 0, 0, crc, len(stored), len(data),
            len(name_bytes), 0, 0, 0, 0, 0, offset,
        )
        central += name_bytes
    eocd = struct.pack(
        "<IHHHHIIH", 0x06054B50, 0, 0, len(entries), len(entries), len(central), len(local), 0,
    )
    return local + central + eocd


_MANIFEST = (
    b'[app]\nname = "Demo"\nsystem_name = "org.lazy.demo"\nauthor = "Tester"\nversion = "1.0.0"\n'
    b'\n[entry]\nbinary = "bin/app.elf"\n'
)
_PNG = b"\x89PNG\r\n\x1a\n" + b"\x00\x00\x00\x0dIHDR"


def lazypkg_seeds():
    members = [
        ("manifest.toml", _MANIFEST, False),
        ("bin/app.elf", b"ELF fake binary", False),
        ("icons/app-16.png", _PNG, False),
        ("icons/app-32.png", _PNG, False),
        ("icons/app-128.png", _PNG, False),
    ]
    valid_stored = _zip(members)
    valid_deflated = _zip([(name, data, True) for name, data, _ in members])
    bad_path = _zip(members + [("../evil", b"escape", False)])
    return {
        "valid_stored": valid_stored,
        "valid_deflated": valid_deflated,
        "bad_path": bad_path,
        "truncated": valid_stored[: len(valid_stored) // 2],
    }


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


#: Destination bytes each selector reads before the length (`fuzz.rs`): 0 is the
#: gateway, 1 four free octets, 2 the last octet of 10.0.2.x, 3 the last two of
#: 192.168.x.y.
NS_DST_BYTES = {0: 0, 1: 4, 2: 1, 3: 2}


def ns_ping(dst_kind=0, length=56, timeout=50, dst=None):
    if dst is None:
        dst = {0: b"", 1: bytes([127, 0, 0, 1]), 2: bytes([2]), 3: bytes([1, 1])}[dst_kind]
    assert len(dst) == NS_DST_BYTES[dst_kind], "destination bytes do not match the selector"
    return bytes([150, dst_kind]) + dst + struct.pack(">H", length) + bytes([timeout])


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
        "ping_abuse": ns_header() + ns_step() * 12
        + ns_ping(dst_kind=1, length=8, timeout=10)  # loopback: refused up front
        + ns_ping(dst_kind=2, length=0, timeout=50)  # the gateway's neighbour, empty payload
        + ns_ping(dst_kind=3, length=1400, timeout=255)  # off-link, largest payload
        + ns_ping(dst_kind=0, length=1500, timeout=1)  # one over the payload limit
        + b"".join(ns_ping(dst_kind=2, dst=bytes([100 + i]), length=8, timeout=255) for i in range(9))  # past the cap
        + ns_step() * 8,
        # Cancel a ping, flip the gateway's echo behaviour.
        "cancel_and_mute": ns_header() + ns_step() * 12 + ns_ping() + bytes([182, 0, 0]) + bytes([172, 1]) + ns_ping() + ns_step() * 8,
        "empty": b"",
    }


# ---- inputmap pointer grammar -------------------------------------------------
#
# `libs/inputmap/src/fuzz.rs`: width and height (u16 LE), then ops. Each op is
# a byte (low 3 bits select it, bit 7 flushes after it) and a device byte.

P_REL, P_ABS, P_BUTTON, P_SCROLL, P_DROPPED, P_BOUNDS, P_RAW = 0, 1, 2, 4, 5, 6, 7
P_FLUSH = 0x80


def p_screen(width, height):
    return struct.pack("<HH", width, height)


def p_rel(dx, dy, device=2, flush=False):
    return bytes([P_REL | (P_FLUSH if flush else 0), device]) + struct.pack("<hh", dx, dy)


def p_abs(x, y, device=3):
    return bytes([P_ABS, device]) + struct.pack("<HH", x, y)


def p_button(usage, value, device=2):
    return bytes([P_BUTTON, device, usage, value])


def p_scroll(axis, notches, device=2):
    return bytes([P_SCROLL, device, axis]) + struct.pack("<b", notches)


def p_raw(kind, code, value, device=2):
    return bytes([P_RAW, device, kind]) + struct.pack("<Hi", code, value)


def inputmap_pointer_seeds():
    return {
        # Every edge of an 800x600 screen.
        "edges": p_screen(800, 600) + b"".join(p_rel(dx, dy, flush=True) for dx, dy in
                                                ((-32768, 0), (0, -32768), (32767, 0), (0, 32767), (-5, -5))),
        # Absolute corners, then a resize that re-clamps.
        "absolute_resize": p_screen(1024, 768) + p_abs(0xFFFF, 0xFFFF) + p_abs(0, 0) + p_abs(0x8000, 0x8000)
        + bytes([P_BOUNDS, 0]) + struct.pack("<HH", 320, 200) + p_abs(0xFFFF, 0xFFFF),
        # Two devices hold one button; a loss marker releases everything.
        "two_devices_dropped": p_screen(640, 480) + p_button(1, 1, 2) + p_button(1, 1, 3) + p_button(1, 0, 2)
        + p_rel(3, 3) + bytes([P_DROPPED, 0]) + p_button(1, 0, 3),
        # Wheel on both axes around button edges.
        "wheel": p_screen(640, 480) + p_scroll(0, 3) + p_button(2, 1) + p_scroll(0, -1) + p_scroll(1, 2)
        + p_button(2, 0) + p_rel(0, 0, flush=True),
        # Hostile records: wrong codes, non-edge values, foreign kinds.
        "hostile": p_screen(1, 1) + p_raw(4, 0, 1) + p_raw(4, 6, 1) + p_raw(4, 1, 2) + p_raw(5, 2, 1)
        + p_raw(2, 1, 5) + p_raw(1, 4, 1) + p_raw(200, 0xFFFF, -1) + p_raw(5, 0, 0x7FFFFFFF),
        "degenerate_screen": p_screen(0, 0) + p_rel(100, 100, flush=True) + p_abs(0xFFFF, 0xFFFF),
        "empty": b"",
    }


# ---- usbhid (libs/usbhid/src/fuzz.rs) ---------------------------------------
#
# `usbdesc` takes raw descriptor bytes; the QEMU HID devices' high-speed
# configurations (libs/usbhid/src/tests/golden.rs) plus hostile edits.

KBD_CONFIG = bytes([9, 2, 34, 0, 1, 1, 8, 0xA0, 50, 9, 4, 0, 0, 1, 3, 1, 1, 0,
                    9, 0x21, 0x11, 0x01, 0, 1, 0x22, 0x3F, 0, 7, 5, 0x81, 3, 8, 0, 7])
MOUSE_CONFIG = bytes([9, 2, 34, 0, 1, 1, 6, 0xA0, 50, 9, 4, 0, 0, 1, 3, 1, 2, 0,
                      9, 0x21, 0x01, 0x00, 0, 1, 0x22, 52, 0, 7, 5, 0x81, 3, 4, 0, 7])
TABLET_CONFIG = bytes([9, 2, 34, 0, 1, 1, 7, 0xA0, 50, 9, 4, 0, 0, 1, 3, 0, 0, 0,
                       9, 0x21, 0x01, 0x00, 0, 1, 0x22, 74, 0, 7, 5, 0x81, 3, 8, 0, 4])
KBD_DEVICE = bytes([18, 1, 0x00, 0x02, 0, 0, 0, 64, 0x27, 0x06, 0x01, 0x00, 0, 0, 1, 4, 11, 1])


def _patched(data, at, value):
    out = bytearray(data)
    out[at] = value
    return bytes(out)


def usbdesc_seeds():
    return {
        "kbd_config": KBD_CONFIG,
        "mouse_config": MOUSE_CONFIG,
        "tablet_config": TABLET_CONFIG,
        "kbd_device": KBD_DEVICE,
        "zero_blength": _patched(KBD_CONFIG, 9, 0),
        "record_overrun": _patched(KBD_CONFIG, 27, 9),
        "total_past_end": _patched(KBD_CONFIG, 2, 200),
        # A composite device: keyboard then mouse interface in one chain.
        "composite": bytes([9, 2, 59, 0, 2, 1, 0, 0xA0, 50]) + KBD_CONFIG[9:] + MOUSE_CONFIG[9:11]
        + bytes([1]) + MOUSE_CONFIG[12:],
        "empty": b"",
    }


def _report(selector, data):
    return bytes([selector, len(data)]) + bytes(data)


def hidreport_seeds():
    kbd, mouse = 0, 1
    return {
        "typing": _report(kbd, [0x02, 0, 0x04, 0, 0, 0, 0, 0]) + _report(kbd, [0x02, 0, 0x04, 0x05, 0, 0, 0, 0])
        + _report(kbd, [0, 0, 0x05, 0, 0, 0, 0, 0]) + _report(kbd, [0, 0, 0, 0, 0, 0, 0, 0]),
        "rollover": _report(kbd, [0, 0, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09]) + _report(kbd, [0, 0, 1, 1, 1, 1, 1, 1])
        + _report(kbd, [0, 0, 0, 0, 0, 0, 0, 0]),
        "hostile_keys": _report(kbd, [0xFF, 0, 0xFF, 0xE8, 0x03, 0x04, 0, 0]) + _report(kbd, [0x00])
        + _report(kbd, [0x01, 0, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04, 0x05, 0x06, 0x07]),
        "mouse": _report(mouse, [0x01, 5, 0xFB, 0x01]) + _report(mouse, [0x03, 0, 0]) + _report(mouse, [0xFF, 0x80, 0x7F, 0x80])
        + _report(mouse, [0, 0]) + _report(mouse, [0, 0, 0, 0]),
        "empty": b"",
    }


# `hidreportdesc`: a length byte, then a report descriptor and that many
# report bytes (QEMU's tablet and mouse, a report-id device, hostile edits).
TABLET_REPORT = bytes([
    0x05, 0x01, 0x09, 0x02, 0xA1, 0x01, 0x09, 0x01, 0xA1, 0x00, 0x05, 0x09, 0x19, 0x01, 0x29,
    0x05, 0x15, 0x00, 0x25, 0x01, 0x95, 0x05, 0x75, 0x01, 0x81, 0x02, 0x95, 0x01, 0x75, 0x03,
    0x81, 0x01, 0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x15, 0x00, 0x26, 0xFF, 0x7F, 0x35, 0x00,
    0x46, 0xFF, 0x7F, 0x75, 0x10, 0x95, 0x02, 0x81, 0x02, 0x05, 0x01, 0x09, 0x38, 0x15, 0x81,
    0x25, 0x7F, 0x35, 0x00, 0x45, 0x00, 0x75, 0x08, 0x95, 0x01, 0x81, 0x06, 0xC0, 0xC0])
MOUSE_REPORT = bytes([
    0x05, 0x01, 0x09, 0x02, 0xA1, 0x01, 0x09, 0x01, 0xA1, 0x00, 0x05, 0x09, 0x19, 0x01, 0x29,
    0x05, 0x15, 0x00, 0x25, 0x01, 0x95, 0x05, 0x75, 0x01, 0x81, 0x02, 0x95, 0x01, 0x75, 0x03,
    0x81, 0x01, 0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x09, 0x38, 0x15, 0x81, 0x25, 0x7F, 0x75,
    0x08, 0x95, 0x03, 0x81, 0x06, 0xC0, 0xC0])


def hidreportdesc_seeds():
    two_reports = bytes([
        0x85, 0x01, 0x05, 0x07, 0x19, 0xE0, 0x29, 0xE7, 0x15, 0x00, 0x25, 0x01, 0x75, 0x01, 0x95,
        0x08, 0x81, 0x02, 0x85, 0x02, 0x05, 0x09, 0x19, 0x01, 0x29, 0x03, 0x95, 0x03, 0x75, 0x01,
        0x81, 0x02, 0x95, 0x01, 0x75, 0x05, 0x81, 0x03, 0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x15,
        0x81, 0x25, 0x7F, 0x75, 0x08, 0x95, 0x02, 0x81, 0x06])
    return {
        "tablet": bytes([6]) + TABLET_REPORT + bytes([0x01, 0x00, 0x40, 0xFF, 0x7F, 0xFF]),
        "mouse": bytes([4]) + MOUSE_REPORT + bytes([0x06, 0xFE, 0x05, 0x01]),
        "report_ids": bytes([4]) + two_reports + bytes([2, 1, 3, 0xFD]),
        "push_pop": bytes([0]) + bytes([0xA4] * 8 + [0xB4] * 12) + TABLET_REPORT,
        "truncated": bytes([0]) + TABLET_REPORT[:41],
        "long_item": bytes([0]) + bytes([0xFE, 0x05, 0x10]) + TABLET_REPORT,
        "empty": b"",
    }


# ---- ext2fs script grammar ----------------------------------------------------
# `libs/ext2fs/src/fuzz.rs`: a head byte (bit 0 picks corruption mode, bit 1 the
# block size: clear 1 KiB, set 4 KiB), then either four-byte operations
# `kind a b c` (model mode) or three-byte `hi lo value` byte overwrites of the
# image's first 48 KiB (corruption mode).

MKDIR, CREATE, WRITE, TRUNCATE, UNLINK, RMDIR, RENAME, READ = range(8)


def e2_op(kind, a=0, b=0, c=0):
    return bytes([kind, a, b, c])


def e2_poke(at, value):
    return bytes([at >> 8, at & 0xFF, value])


def ext2fs_seeds():
    # a=1 names /d0, a=2 /d1, a=0 the root; b picks the file (b % 4) or the directory (b % 2).
    tree = e2_op(MKDIR, 0, 0) + e2_op(MKDIR, 0, 1)
    workout = (
        tree
        + e2_op(CREATE, 1, 0) + e2_op(CREATE, 1, 1) + e2_op(CREATE, 0, 2)
        + e2_op(WRITE, 1, 0, 40) + e2_op(WRITE, 1, 1, 200) + e2_op(WRITE, 0, 2, 255)
        + e2_op(READ, 1, 0) + e2_op(TRUNCATE, 1, 0, 3) + e2_op(WRITE, 1, 100, 80)
        + e2_op(RENAME, 1, 1, 2) + e2_op(READ, 2, 1) + e2_op(UNLINK, 0, 2)
        + e2_op(RMDIR, 0, 1) + e2_op(UNLINK, 2, 1) + e2_op(RMDIR, 0, 1)
    )
    # 1 KiB blocks: the second file crosses into the single-indirect range.
    indirect = tree + e2_op(CREATE, 1, 0) + e2_op(WRITE, 1, 0, 250) + e2_op(WRITE, 1, 200, 250) + e2_op(READ, 1, 0)
    churn = tree + b"".join(
        e2_op(CREATE, 1 + i % 2, i) + e2_op(WRITE, 1 + i % 2, i, 60 + i) + e2_op(UNLINK, 1 + i % 2, i)
        for i in range(24)
    )
    errors = (
        e2_op(CREATE, 1, 0)  # no /d0 yet
        + e2_op(UNLINK, 0, 0) + e2_op(RMDIR, 0, 0) + e2_op(READ, 0, 0) + e2_op(TRUNCATE, 0, 0, 5)
        + tree + e2_op(MKDIR, 0, 0) + e2_op(RENAME, 1, 0, 0) + e2_op(RENAME, 1, 0, 1)
    )
    sb = 1024  # superblock byte offset; descriptors follow in block 2 (1 KiB blocks)
    gdt = 2048
    return {
        "model_workout_1k": bytes([0]) + workout,
        "model_workout_4k": bytes([2]) + workout,
        "model_indirect_1k": bytes([0]) + indirect,
        "model_indirect_4k": bytes([2]) + indirect,
        "model_churn": bytes([0]) + churn,
        "model_errors": bytes([0]) + errors,
        "corrupt_magic": bytes([1]) + e2_poke(sb + 0x38, 0),
        "corrupt_log_block_size": bytes([1]) + e2_poke(sb + 0x18, 3),
        "corrupt_counts": bytes([1]) + e2_poke(sb + 0x07, 0x7F) + e2_poke(sb + 0x0F, 0xFF),
        # Groups with more bits than one bitmap block holds (must be refused at mount).
        "corrupt_blocks_per_group": bytes([1]) + e2_poke(sb + 0x21, 0xFF) + e2_poke(sb + 0x22, 0x01),
        "corrupt_inodes_per_group": bytes([1]) + e2_poke(sb + 0x29, 0x80),
        "corrupt_incompat": bytes([1]) + e2_poke(sb + 0x60, 0x42),
        "corrupt_inode_table": bytes([1]) + e2_poke(gdt + 8, 0xFF) + e2_poke(gdt + 11, 0x7F),
        "corrupt_bitmaps": bytes([1]) + e2_poke(gdt + 0, 0x01) + e2_poke(gdt + 4, 0x01),
        "corrupt_dir_records": bytes([1]) + e2_poke(2048 + 32 * 4 + 4, 0) + e2_poke(2048 + 32 * 4 + 6, 0xC8),
        "corrupt_none": bytes([1]),
        "empty": b"",
    }


# ---- acpi: golden firmware dumps as physical-memory images ---------------------

ACPI_GOLDEN = Path(__file__).resolve().parent.parent / "libs" / "acpi" / "golden"


def acpi_seeds():
    """`libs/acpi/src/fuzz.rs` input: a mode byte (1 = re-seal checksums) and
    the body of a golden dump (`tools/acpi/dump_tables.py`) without its magic."""
    seeds = {}
    for dump in sorted(ACPI_GOLDEN.glob("*.bin")):
        body = dump.read_bytes()[8:]
        seeds[dump.stem] = bytes([0]) + body
        seeds[dump.stem + "_sealed"] = bytes([1]) + body
    seeds["empty"] = b""
    seeds["rsdp_only"] = bytes([1]) + struct.pack("<Q", 0x1000)
    return seeds


TARGETS = {
    "acpi": acpi_seeds,
    "ext2fs": ext2fs_seeds,
    "framering": framering_seeds,
    "framering_header": header_seeds,
    "virtio_net": virtio_net_seeds,
    "nicdrv": nicdrv_seeds,
    "lazypkg": lazypkg_seeds,
    "netstack": netstack_seeds,
    "inputmap_pointer": inputmap_pointer_seeds,
    "usbdesc": usbdesc_seeds,
    "hidreport": hidreport_seeds,
    "hidreportdesc": hidreportdesc_seeds,
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
