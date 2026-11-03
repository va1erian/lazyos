"""Seeds for the `btgatt` fuzz target (`gen_corpus.py`).

The input is one ATT PDU (parsed as the answer to every kind of request, and
as an unsolicited PDU) and, read as a script, a flags octet (bit 0: resume
with the stored CCCD handles instead of discovering) then frames, each a
length octet and the bytes, delivered to the HID over GATT driver as the
server's answers (`libs/btgatt/src/fuzz.rs`). A frame whose first octet is
0x00 is given the response opcode of the request in flight, so a script
reaches the deep states instead of stopping at the opcode check.
"""

# ATT opcodes (Core Specification, Vol 3 Part F).
ERROR = 0x01
READ_BY_TYPE_REQ, READ_BY_GROUP_REQ, FIND_INFO_REQ = 0x08, 0x10, 0x04


def u16(v):
    return v.to_bytes(2, "little")


def frame(pdu):
    return bytes([len(pdu)]) + pdu


def script(flags, *pdus):
    return bytes([flags]) + b"".join(frame(p) for p in pdus)


def error(request, handle, code=0x0A):
    return bytes([ERROR, request]) + u16(handle) + bytes([code])


# What a small keyboard answers. The patched first octet (0x00) becomes the
# response opcode of whichever request is in flight.
MTU = bytes([0]) + u16(64)
HID_GROUP = bytes([0, 6]) + u16(0x20) + u16(0x33) + u16(0x1812)
GAP_GROUP = bytes([0, 6]) + u16(1) + u16(3) + u16(0x1800)
CHARS = bytes([0, 7]) + b"".join(
    u16(decl) + bytes([props]) + u16(value) + u16(uuid)
    for decl, props, value, uuid in [
        (0x22, 0x02, 0x23, 0x2A4A),
        (0x24, 0x02, 0x25, 0x2A4B),
        (0x28, 0x06, 0x29, 0x2A4E),
        (0x2A, 0x12, 0x2B, 0x2A4D),
    ]
)
DESCRIPTORS = bytes([0, 1]) + u16(0x2C) + u16(0x2902) + u16(0x2D) + u16(0x2908)
REPORT_REFERENCE = bytes([0, 1, 1])
HID_INFORMATION = bytes([0, 0x11, 0x01, 0x00, 0x02])
REPORT_MAP = bytes([0, 0x05, 0x01, 0x09, 0x06, 0xA1, 0x01])
WRITE_RESPONSE = bytes([0])


def seeds():
    discovery = [
        MTU,
        HID_GROUP,
        CHARS,
        error(READ_BY_TYPE_REQ, 0x2B),
        DESCRIPTORS,
        error(FIND_INFO_REQ, 0x2E),
        REPORT_REFERENCE,
        HID_INFORMATION,
        REPORT_MAP,
    ]
    wide_uuid = bytes([0, 2]) + u16(1) + bytes(range(16))
    return {
        "discovery_complete": script(0, *discovery),
        "discovery_truncated": script(0, *discovery[:5]),
        "resume": script(1, WRITE_RESPONSE, WRITE_RESPONSE),
        "resume_refused": script(1, error(0x12, 0x2C, 0x0F)),
        "no_hid_service": script(0, MTU, GAP_GROUP, error(READ_BY_GROUP_REQ, 4)),
        "mtu_not_supported": script(0, error(0x02, 0, 0x06), HID_GROUP),
        "long_report_map": script(
            0, *discovery[:8], bytes([0]) + bytes(22), bytes([0]) + bytes(22), bytes([0, 1])
        ),
        "notification_mid_discovery": script(0, MTU, bytes([0x1B]) + u16(0x2B) + bytes(8), HID_GROUP),
        "group_not_ascending": script(0, MTU, HID_GROUP, GAP_GROUP),
        "pdu_error_response": error(0x0A, 3, 0x0F),
        "pdu_notification": bytes([0x1B]) + u16(0x2B) + bytes([0, 0, 4, 0, 0, 0, 0, 0]),
        "pdu_indication": bytes([0x1D]) + u16(0x2B) + b"\x01",
        "pdu_server_request": bytes([0x0A]) + u16(3),
        "pdu_group_ragged": HID_GROUP + b"\x00",
        "pdu_find_info_128": bytes([0, 2]) + u16(1) + bytes(range(16)),
        "pdu_characteristics_wide_uuid": wide_uuid,
        "empty": b"",
    }
