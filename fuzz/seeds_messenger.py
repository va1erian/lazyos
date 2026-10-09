"""Seeds for the Messenger parcel decoder (`libmessenger::fuzz::run`): parcel
version 2 bytes (`docs/messenger-core-plan.md` 3.1), built here so the seed
set does not depend on the Rust encoder it tests.
"""
import struct

VERSION = 2
HANDLE, BUFFER, STRING, U32, STRUCT, OPTION = 14, 15, 7, 4, 10, 12
CHANNEL, BUF = 1, 2


def tlv(kind, field_id, payload):
    return struct.pack("<II", kind | (field_id << 8), len(payload)) + payload


def channel_field(field_id, index):
    return tlv(HANDLE, field_id, struct.pack("<I", index))


def buffer_field(field_id, index, offset=0, length=4096):
    return tlv(BUFFER, field_id, struct.pack("<IQQ", index, offset, length))


def parcel(body, objects=(), flags=1, interface=0x1234, method=7, version=VERSION):
    """`objects` is a list of `(kind, handle)`; the header is 48 bytes."""
    header = struct.pack(
        "<HHIQIIQQQ", version, flags, len(objects), interface, method, len(body), 0xFEED, 0, 0
    )
    entries = b"".join(struct.pack("<IIQ", kind, 0, handle) for kind, handle in objects)
    return header + body + entries


def messenger_seeds():
    value = tlv(U32, 1, struct.pack("<I", 42)) + tlv(STRING, 2, b"seed")
    good = parcel(value + channel_field(3, 0) + buffer_field(4, 1), [(CHANNEL, 5), (BUF, 9)])
    seeds = {
        "empty_body": parcel(b""),
        "values_only": parcel(value),
        "channel_and_buffer": good,
        # The object fields inside a nested struct, still in declared order.
        "objects_in_struct": parcel(
            value + tlv(STRUCT, 3, channel_field(1, 0) + buffer_field(2, 1)),
            [(CHANNEL, 5), (BUF, 9)],
        ),
        # Index rule violations: repeated, skipped, out of range, wrong kind.
        "index_repeated": parcel(channel_field(1, 0) + channel_field(2, 0), [(CHANNEL, 5), (CHANNEL, 6)]),
        "index_skipped": parcel(channel_field(1, 1) + channel_field(2, 0), [(CHANNEL, 5), (CHANNEL, 6)]),
        "index_out_of_range": parcel(buffer_field(1, 3), [(BUF, 9)]),
        "index_wrong_kind": parcel(channel_field(1, 0), [(BUF, 9)]),
        "unclaimed_object": parcel(value, [(BUF, 9)]),
        # Object list faults: an unknown kind, a reserved word, a count past the list.
        "unknown_kind": good[:-16] + struct.pack("<IIQ", 3, 0, 9),
        "reserved_set": good[:-16] + struct.pack("<IIQ", BUF, 1, 9),
        "count_past_list": good[:4] + struct.pack("<I", 3) + good[8:],
        "eight_objects": parcel(
            b"".join(channel_field(i + 1, i) for i in range(8)), [(CHANNEL, i) for i in range(8)]
        ),
        "nine_objects": parcel(b"", [(CHANNEL, i) for i in range(9)]),
        # Malformed fields: short handle payload, short buffer payload, an option
        # wrapping an object, a truncated TLV.
        "short_handle": parcel(tlv(HANDLE, 1, b"\x00\x00"), [(CHANNEL, 5)]),
        "short_buffer": parcel(tlv(BUFFER, 1, b"\x00" * 8), [(BUF, 9)]),
        "option_object": parcel(tlv(OPTION, 1, channel_field(1, 0)), [(CHANNEL, 5)]),
        "truncated_tlv": parcel(tlv(U32, 1, b"\x01\x02\x03\x04")[:-2] + b"", []),
        "bad_version": parcel(value, version=1),
        "trailing_bytes": good + b"\x00",
        "header_only": good[:48],
        "short_header": good[:30],
    }
    return seeds
