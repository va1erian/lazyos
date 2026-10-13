"""Seeds for the `sntp` fuzz target (`gen_corpus.py`).

The target's first byte picks the half (`libs/sntp/src/fuzz.rs`). Even: a
reply, `[0][sent: i64 LE ns][rtt: u64 LE ns][48-byte packet]`, where the
packet's own originate field is used as the nonce. Odd: a schedule script of
`[op, arg]` pairs (0 up, 1 down, 2 query if due, 3 succeeded, 4 failed,
5 rate limited, 6 sync now, 7 an hour passes).
"""
import struct

NTP_UNIX = 2_208_988_800
NS = 1_000_000_000
NOW = 1_791_849_600 * NS  # 2026-10-13T00:00:00Z
NONCE = 0x0123456789ABCDEF


def ts(unix_ns):
    """An NTP 32.32 timestamp of a Unix instant in nanoseconds."""
    secs = (unix_ns // NS + NTP_UNIX) & 0xFFFFFFFF
    frac = ((unix_ns % NS) << 32) // NS
    return struct.pack(">II", secs, frac)


def packet(t2, t3, *, li=0, vn=4, mode=4, stratum=2, refid=b"LAZY", nonce=NONCE,
           root_delay=0, root_disp=0):
    head = bytes([(li << 6) | (vn << 3) | mode, stratum, 6, 0xEC])
    return (head + struct.pack(">II", root_delay, root_disp) + refid
            + b"\0" * 8 + struct.pack(">Q", nonce) + t2 + t3)


def reply(sent, rtt, pkt):
    return b"\x00" + struct.pack("<qQ", sent, rtt) + pkt


def script(*ops):
    return b"\x01" + bytes(b for op in ops for b in op)


def sntp_seeds():
    t2 = NOW + 10 * NS + 20_000_000
    good = packet(ts(t2), ts(t2 + 1_000_000))
    rollover = ((1 << 32) - NTP_UNIX) * NS
    seeds = {
        "reply_good": reply(NOW, 42_000_000, good),
        "reply_dead_rtc": reply(946_684_800 * NS, 42_000_000,
                                packet(ts(NOW), ts(NOW + 1_000_000))),
        "reply_before_floor": reply(NOW, 42_000_000, packet(ts(0 + NS), ts(NS))),
        "reply_kiss_rate": reply(NOW, 40_000_000, packet(ts(t2), ts(t2), stratum=0, refid=b"RATE")),
        "reply_kiss_deny": reply(NOW, 40_000_000, packet(ts(t2), ts(t2), stratum=0, refid=b"DENY")),
        "reply_alarm": reply(NOW, 40_000_000, packet(ts(t2), ts(t2), li=3)),
        "reply_stratum_16": reply(NOW, 40_000_000, packet(ts(t2), ts(t2), stratum=16)),
        "reply_client_mode": reply(NOW, 40_000_000, packet(ts(t2), ts(t2), mode=3)),
        "reply_version_2": reply(NOW, 40_000_000, packet(ts(t2), ts(t2), vn=2)),
        "reply_zero_timestamps": reply(NOW, 40_000_000, packet(b"\0" * 8, b"\0" * 8)),
        "reply_backwards": reply(NOW, 40_000_000, packet(ts(t2 + NS), ts(t2))),
        "reply_negative_root_delay": reply(NOW, 40_000_000, packet(ts(t2), ts(t2), root_delay=0x80000000)),
        "reply_root_distance": reply(NOW, 40_000_000, packet(ts(t2), ts(t2), root_disp=0x00050000)),
        "reply_huge_rtt": reply(NOW, 5 * NS, good),
        "reply_rollover": reply(rollover - 6 * NS, 0,
                                packet(ts(rollover + NS), ts(rollover + NS))),
        "reply_with_extension": reply(NOW, 42_000_000, good + b"\xAA" * 20),
        "reply_short": reply(NOW, 42_000_000, good[:47]),
        "reply_extremes": reply(-(1 << 63), (1 << 64) - 1,
                                packet(b"\xFF" * 8, b"\xFF" * 8)),
        # A reply that is valid but for a nonce another request carried.
        "reply_wrong_nonce_prefix": reply(NOW, 42_000_000, good[:24] + b"\0" * 8 + good[32:]),
        "schedule_up_query_success": script(
            (0, 0), (2, 0), (3, 0), (2, 0), (7, 0), (2, 0), (3, 1)),
        "schedule_backoff": script(
            (0, 0), (2, 20), (4, 0), (2, 20), (4, 0), (2, 40), (4, 0), (2, 60), (4, 0),
            (2, 99), (4, 0), (2, 99), (4, 0), (3, 7)),
        "schedule_rate_limited": script(
            (0, 0), (2, 0), (5, 0), (7, 0), (2, 0), (5, 0), (7, 0), (7, 0), (2, 0)),
        "schedule_flapping_link": script(
            (0, 0), (1, 1), (0, 1), (1, 1), (0, 1), (2, 0), (3, 3), (1, 0), (0, 0), (2, 5)),
        "schedule_sync_now_spam": script(
            (0, 0), (6, 0), (2, 0), (6, 0), (6, 1), (2, 1), (6, 40), (2, 0)),
    }
    return seeds
