"""Seeds for the `bthci`, `btl2cap` and `btbond` fuzz targets (`gen_corpus.py`).

`bthci` (libs/bthci/src/fuzz.rs) uses its input as one event packet, one
command packet, one ACL packet, and as a script of `u8` length-prefixed event
packets fed to the init state machine, the command queue and the ACL buffer
pool. The seeds are real packets of each kind plus a script that completes the
init sequence for a controller with one accept-list entry.

`btl2cap` (libs/btl2cap/src/fuzz.rs) uses its input as a signalling payload and
as a script: one head octet (bit 0 LE link, bit 1 HID PSMs allowed), then
records of `boundary:u8, length:u8, bytes`.

`btbond` (libs/btbond/src/fuzz.rs) takes a bond file. The valid seeds here are
built by an independent encoder and libs/btbond/src/tests.rs checks they are
byte-identical to what the Rust encoder writes for the same bonds.
"""
import struct
import zlib


def opcode(ogf, ocf):
    return ogf << 10 | ocf


def ev(code, params=b""):
    return bytes([code, len(params)]) + params


def cmd(op, params=b""):
    return struct.pack("<HB", op, len(params)) + params


def cc(op, rp=b"\x00"):
    return ev(0x0E, b"\x01" + struct.pack("<H", op) + rp)


def cs(op, status=0):
    return ev(0x0F, bytes([status, 1]) + struct.pack("<H", op))


ADDR = bytes([0x11, 0x22, 0x33, 0x44, 0x55, 0x66])


def le_meta(sub, params):
    return ev(0x3E, bytes([sub]) + params)


def frames(*items):
    return b"".join(bytes([len(i)]) + i for i in items)


def init_script():
    """Command Complete for every command Init sends with one accept-list entry."""
    ops = [
        (opcode(3, 0x03), b"\x00"),
        (opcode(4, 0x05), b"\x00" + struct.pack("<HBHH", 339, 0, 12, 0)),
        (opcode(3, 0x01), b"\x00"),
        (opcode(8, 0x02), b"\x00" + struct.pack("<HB", 251, 8)),
        (opcode(8, 0x01), b"\x00"),
        (opcode(3, 0x56), b"\x00"),
        (opcode(3, 0x1A), b"\x00"),
        (opcode(8, 0x10), b"\x00"),
        (opcode(8, 0x11), b"\x00"),
    ]
    out = [cc(op, rp) for op, rp in ops]
    out.append(cs(opcode(8, 0x0D)))
    out.append(cc(opcode(3, 0x24)))
    out.append(cc(opcode(3, 0x13)))
    out.append(cc(opcode(4, 0x09), b"\x00" + ADDR))
    out.append(cc(opcode(4, 0x01), b"\x00" + struct.pack("<BHBHH", 9, 1, 9, 2, 3)))
    out.append(ev(0x13, b"\x01" + struct.pack("<HH", 0x40, 1)))
    out.append(ev(0x05, b"\x00" + struct.pack("<H", 0x40) + b"\x13"))
    return frames(*out)


def bthci_seeds():
    return {
        "empty": b"",
        "event_command_complete": cc(opcode(3, 0x03)),
        "event_command_status": cs(opcode(1, 0x05)),
        "event_connection_request": ev(0x04, ADDR + b"\x40\x05\x00\x01"),
        "event_connection_complete": ev(0x03, b"\x00" + struct.pack("<H", 0x40) + ADDR + b"\x01\x00"),
        "event_disconnection": ev(0x05, b"\x00" + struct.pack("<H", 0x40) + b"\x13"),
        "event_encryption_change": ev(0x08, b"\x00" + struct.pack("<H", 0x40) + b"\x01"),
        "event_link_key_notification": ev(0x18, ADDR + b"\x07" * 16 + b"\x05"),
        "event_user_passkey": ev(0x3B, ADDR + struct.pack("<I", 123456)),
        "event_completed_packets": ev(0x13, b"\x02" + struct.pack("<HHHH", 0x40, 0x41, 1, 3)),
        "event_completed_packets_lying": ev(0x13, b"\xc8" + struct.pack("<HH", 0x40, 1)),
        "event_le_connection": le_meta(1, b"\x00" + struct.pack("<H", 0x40) + b"\x00\x01" + ADDR + struct.pack("<HHH", 24, 0, 400) + b"\x00"),
        "event_le_enhanced": le_meta(0x0A, b"\x00" + struct.pack("<H", 0x40) + b"\x00\x01" + ADDR + b"\x01" * 6 + b"\x02" * 6 + struct.pack("<HHH", 24, 0, 400) + b"\x00"),
        "event_le_adv_report": le_meta(2, b"\x01\x00\x01" + ADDR + b"\x03\x02\x01\x06" + b"\xc4"),
        "event_le_adv_report_oversize": le_meta(2, b"\x01\x00\x01" + ADDR + b"\x20" + b"\x00" * 33),
        "event_le_ltk_request": le_meta(5, struct.pack("<H", 0x40) + bytes(range(8)) + struct.pack("<H", 0x1234)),
        "command_reset": cmd(opcode(3, 0x03)),
        "command_set_event_mask": cmd(opcode(3, 0x01), b"\xff\xff\xfb\xff\x07\xf8\xbf\x3d"),
        "command_create_connection": cmd(opcode(1, 0x05), ADDR + struct.pack("<HBBHB", 0xCC18, 2, 0, 0, 1)),
        "command_le_create_connection": cmd(opcode(8, 0x0D), struct.pack("<HHBB", 0x60, 0x30, 1, 0) + ADDR + b"\x00" + struct.pack("<HHHHHH", 24, 40, 0, 400, 0, 0)),
        "command_write_local_name": cmd(opcode(3, 0x13), b"LazyOS".ljust(248, b"\x00")),
        "command_ltk_reply": cmd(opcode(8, 0x1A), struct.pack("<H", 0x40) + b"\xa5" * 16),
        "acl_first": struct.pack("<HH", 0x40 | 0x2000, 3) + b"abc",
        "acl_continuing": struct.pack("<HH", 0x40 | 0x1000, 2) + b"de",
        "script_init": init_script(),
    }


def sig(code, ident, data):
    return bytes([code, ident]) + struct.pack("<H", len(data)) + data


def pdu(cid, payload):
    return struct.pack("<HH", len(payload), cid) + payload


def records(*items, head=2):
    out = bytes([head])
    for boundary, data in items:
        out += bytes([boundary, len(data)]) + data
    return out


def btl2cap_seeds():
    mtu = b"\x01\x02\xa0\x02"
    conn_req = sig(2, 1, struct.pack("<HH", 0x11, 0x50))
    conf_req = sig(4, 2, struct.pack("<HH", 0x40, 0) + mtu)
    conf_rsp = sig(5, 1, struct.pack("<HHH", 0x40, 0, 0))
    disc_req = sig(6, 3, struct.pack("<HH", 0x40, 0x50))
    open_close = records(
        (0, pdu(1, conn_req)),
        (0, pdu(1, conf_req)),
        (0, pdu(1, conf_rsp)),
        (0, pdu(0x40, b"\xa1\x01\x02")),
        (0, pdu(1, disc_req)),
    )
    whole = pdu(1, conn_req)
    split = records((0, whole[:3]), (1, whole[3:]))
    le = records(
        (0, pdu(4, b"\x02\x17\x00")),
        (0, pdu(6, b"\x01\x03")),
        (0, pdu(5, sig(0x12, 7, struct.pack("<HHHH", 6, 12, 0, 200)))),
        head=3,
    )
    return {
        "empty": b"",
        "sig_connection_request": conn_req,
        "sig_configuration_request": conf_req,
        "sig_echo_and_information": sig(8, 1, b"hi") + sig(10, 2, struct.pack("<H", 2)),
        "sig_bad_length": bytes([2, 1, 9, 0, 0x11, 0]),
        "basic_pdu": pdu(0x40, b"abc"),
        "script_br_open_close": open_close,
        "script_br_fragmented": split,
        "script_br_refused": records((0, pdu(1, conn_req)), head=0),
        "script_br_oversize": records((0, struct.pack("<HH", 60000, 0x40)), (1, b"x" * 100)),
        "script_le_fixed_channels": le,
    }


# ---- btbond ----------------------------------------------------------------

def bond(addr, kind, flags, key_size, key_type, cod, inp, hid_flags, link_key, ltk, ediv, rand, irk, name, desc, report_map, proto, reports):
    b = bytes(addr) + bytes([kind, flags, key_size, key_type]) + struct.pack("<I", cod)[:3] + bytes([inp, hid_flags])
    b += link_key + ltk + struct.pack("<HQ", ediv, rand) + irk
    b += bytes([len(name)]) + name.encode() + struct.pack("<H", len(desc)) + desc
    b += struct.pack("<HHB", report_map, proto, len(reports))
    for value, cccd, rid, rtype in reports:
        b += struct.pack("<HHBB", value, cccd, rid, rtype)
    return struct.pack("<H", len(b)) + b


Z16 = b"\x00" * 16
DESC = bytes(i % 256 for i in range(300))
AUTH, SC, F_LK, F_LTK, F_IRK = 1, 2, 4, 8, 16


def keyboard_bond():
    return bond([1, 2, 3, 4, 5, 6], 0, AUTH | F_LK, 16, 5, 0x0005C0, 0, 5, b"\x11" * 16, Z16, 0, 0, Z16, "Test Keyboard", DESC, 0, 0, [])


def mouse_bond():
    return bond([9, 8, 7, 6, 5, 4], 0, F_LK, 16, 4, 0x0005C0, 1, 2, b"\x22" * 16, Z16, 0, 0, Z16, "Mouse", bytes([5, 1, 9, 2]), 0, 0, [])


def le_bond():
    reports = [(0x30, 0x31, 1, 1), (0x34, 0x35, 2, 1)]
    return bond([0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xF0], 2, AUTH | SC | F_LTK | F_IRK, 16, 0, 0, 0, 5, Z16, b"\x33" * 16, 0x1234, 0x0102030405060708, b"\x44" * 16, "LE Board", DESC, 0x20, 0x24, reports)


def store(*bonds):
    body = b"LZBT" + struct.pack("<HH", 1, len(bonds)) + b"".join(bonds)
    return body + struct.pack("<I", zlib.crc32(body) & 0xFFFFFFFF)


def btbond_seeds():
    good = store(keyboard_bond(), mouse_bond(), le_bond())
    flipped = bytearray(good)
    flipped[20] ^= 0x40
    return {
        "empty": b"",
        "store_empty": store(),
        "store_keyboard": store(keyboard_bond()),
        "store_three": good,
        "store_bad_crc": bytes(flipped),
        "store_truncated": good[: len(good) // 2],
        # The body of a one-bond file (count, record): the fuzz entry wraps and seals it.
        "body_mouse": struct.pack("<H", 1) + mouse_bond(),
    }
