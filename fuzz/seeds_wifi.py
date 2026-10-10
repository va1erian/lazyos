"""Seeds for the `ieee80211` and `eapol` fuzz targets (`gen_corpus.py`).

`ieee80211` takes a management frame from the MAC header (no FCS); the same
bytes are also tried as an RSN element body and as a scan-table script, so a
few seeds are shaped for those. `eapol` takes one EAPOL-Key PDU, and the same
bytes as a supplicant script: an AKM octet (bit 0 selects PSK-SHA256), then
frames as big-endian `u16` length and PDU. The script runs on the fuzz
crypto (`libs/eapol/src/fuzz.rs`): the MIC is sixteen 0xAA octets and the
"unwrap" drops the first eight octets, so a seed can carry a MIC that passes.
"""
import struct

AP = bytes.fromhex("021122334455")
STA = bytes.fromhex("02aabbccddee")
BCAST = b"\xff" * 6


def ie(tag, body):
    return bytes([tag, len(body)]) + body


def rsn(akm=2, caps=0, extra=b""):
    suite = b"\x00\x0f\xac"
    return ie(48, struct.pack("<H", 1) + suite + b"\x04" + struct.pack("<H", 1) + suite + b"\x04"
              + struct.pack("<H", 1) + suite + bytes([akm]) + struct.pack("<H", caps) + extra)


def beacon_ies(ssid=b"LazyNet", channel=6, rsn_ie=None):
    return (ie(0, ssid) + ie(1, bytes([0x82, 0x84, 0x8B, 0x96, 0x0C, 0x12, 0x18, 0x24]))
            + ie(50, bytes([0x30, 0x48])) + ie(3, bytes([channel])) + ie(7, b"US \x01\x0b\x1e")
            + ie(45, bytes(26)) + ie(191, bytes(12)) + ie(255, bytes([35]) + bytes(20))
            + (rsn() if rsn_ie is None else rsn_ie) + ie(221, b"\x00\x50\xf2\x02\x01\x01"))


def mgmt(subtype, da, sa, bssid, body, flags=0, seq=1):
    return bytes([subtype << 4, flags]) + b"\0\0" + da + sa + bssid + struct.pack("<H", seq << 4) + body


def beacon(ies, subtype=8, da=BCAST, cap=0x0011):
    return mgmt(subtype, da, AP, AP, struct.pack("<QHH", 0x0102030405060708, 100, cap) + ies)


def ieee80211_seeds():
    ies = beacon_ies()
    table = bytes([4]) + b"".join(bytes([op, mac, rssi]) for op, mac, rssi in
                                  [(0, 1, 200), (1, 2, 190), (2, 3, 180), (0, 4, 170), (0, 5, 250),
                                   (3, 0, 5), (1, 1, 100), (3, 0, 0)])
    return {
        "beacon_wpa2": beacon(ies),
        "beacon_hidden_empty": beacon(beacon_ies(b"")),
        "beacon_hidden_nuls": beacon(beacon_ies(b"\0" * 8)),
        "beacon_ssid_too_long": beacon(ie(0, b"a" * 33) + ie(3, b"\x01")),
        "beacon_duplicate_ssid_rsn": beacon(ie(0, b"one") + ie(0, b"two") + rsn(2) + rsn(6)),
        "beacon_truncated_element": beacon(ies)[:-3],
        "beacon_bad_rsn": beacon(ie(0, b"x") + ie(48, b"\x09\x00\x00")),
        "beacon_wpa1_only": beacon(ie(0, b"old") + ie(221, b"\x00\x50\xf2\x01\x01\x00"), cap=0x0011),
        "beacon_open": beacon(ie(0, b"cafe") + ie(3, b"\x0b"), cap=0x0001),
        "beacon_ibss": beacon(ies, cap=0x0002),
        "beacon_many_vendor": beacon(b"".join(ie(221, bytes([0, 1, 2, i])) for i in range(40))),
        "probe_response": beacon(ies, subtype=5, da=STA),
        "probe_request": mgmt(4, BCAST, STA, BCAST, ie(0, b"") + ie(1, b"\x82\x84")),
        "auth_open": mgmt(11, AP, STA, AP, struct.pack("<HHH", 0, 1, 0)),
        "assoc_request": mgmt(0, AP, STA, AP, struct.pack("<HH", 0x0431, 10) + beacon_ies(rsn_ie=rsn())),
        "assoc_response": mgmt(1, STA, AP, AP, struct.pack("<HHH", 0x0431, 0, 0xC001) + ie(1, b"\x82\x84")),
        "deauth": mgmt(12, STA, AP, AP, struct.pack("<H", 15)),
        "disassoc": mgmt(10, STA, AP, AP, struct.pack("<H", 8)),
        "action": mgmt(13, STA, AP, AP, b"\x03\x01\x02\x03"),
        "ht_control": mgmt(12, STA, AP, AP, b"\xaa" * 4 + struct.pack("<H", 4), flags=0x80),
        "protected": mgmt(12, STA, AP, AP, bytes(10), flags=0x40),
        "fragment": mgmt(12, STA, AP, AP, struct.pack("<H", 4), seq=1)[:22] + b"\x11\x00" + struct.pack("<H", 4),
        "data_frame": bytes([0x08, 0x01]) + bytes(40),
        "rsn_body": rsn(2)[2:],
        "rsn_pmkid_gmc": rsn(6, 0x00C0, struct.pack("<H", 1) + bytes(range(16)) + b"\x00\x0f\xac\x06")[2:],
        "table_script": table,
        "empty": b"",
    }


def key_frame(version, flags, key_len, replay, nonce, rsc, key_data, mic=b"\0" * 16, descriptor=2):
    body = (bytes([descriptor]) + struct.pack(">HH", version | flags, key_len) + struct.pack(">Q", replay)
            + nonce + bytes(16) + rsc + bytes(8) + mic + struct.pack(">H", len(key_data)) + key_data)
    return bytes([2, 3]) + struct.pack(">H", len(body)) + body


def pad(data):
    if len(data) % 8 or len(data) < 16:
        data += b"\xdd"
        while len(data) % 8 or len(data) < 16:
            data += b"\0"
    return data


def gtk_kde(index, key):
    return b"\xdd" + bytes([6 + len(key)]) + b"\x00\x0f\xac\x01" + bytes([index & 3, 0]) + key


MIC_OK = b"\xaa" * 16
MSG1, MSG3, GROUP1 = 0x0088, 0x13C8, 0x1380


def script(akm, *frames):
    return bytes([akm]) + b"".join(struct.pack(">H", len(f)) + f for f in frames)


def eapol_seeds():
    out = {}
    for name, akm, version, suite in (("psk", 0, 2, 2), ("sha256", 1, 3, 6)):
        anonce = b"\x11" * 32
        msg1 = key_frame(version, MSG1, 16, 1, anonce, bytes(8), b"")

        def msg3(plain=None, replay=2, nonce=anonce, flags=MSG3, kde=None):
            plain = plain if plain is not None else pad(rsn(suite) + (kde or gtk_kde(1, b"\x22" * 16)))
            return key_frame(version, flags, 16, replay, nonce, b"\x01" * 8, bytes(8) + plain, MIC_OK)

        group = key_frame(version, GROUP1, 16, 3, b"\x44" * 32, b"\x02" * 8,
                          bytes(8) + pad(gtk_kde(2, b"\x33" * 16)), MIC_OK)
        out[f"{name}_handshake"] = script(akm, msg1, msg3(), group)
        out[f"{name}_forged_msg1"] = script(akm, msg1, key_frame(version, MSG1, 16, 900, bytes([0x99]) * 32, bytes(8), b""), msg3(replay=2))
        out[f"{name}_msg3_first"] = script(akm, msg3())
        out[f"{name}_msg3_retransmit"] = script(akm, msg1, msg3(), msg3(replay=4))
        out[f"{name}_msg1_twice"] = script(akm, msg1, key_frame(version, MSG1, 16, 2, anonce, bytes(8), b""), msg3(replay=3))
        out[f"{name}_rsn_mismatch"] = script(akm, msg1, msg3(plain=pad(rsn(suite, 0x00C0) + gtk_kde(1, b"\x22" * 16))))
        out[f"{name}_truncated_kde"] = script(akm, msg1, msg3(plain=pad(rsn(suite) + gtk_kde(1, b"\x22" * 16)[:-6])[:40]))
        out[f"{name}_short_gtk"] = script(akm, msg1, msg3(kde=gtk_kde(1, b"\x22" * 8)))
        out[f"{name}_no_gtk"] = script(akm, msg1, msg3(plain=pad(rsn(suite))))
        out[f"{name}_wrong_flags"] = script(akm, msg1, msg3(flags=MSG3 & ~0x0040))
        out[f"{name}_replayed_counter"] = script(akm, msg1, msg3(replay=1))
        out[f"{name}_other_anonce"] = script(akm, msg1, msg3(nonce=b"\x12" * 32))
        out[f"{name}_group_first"] = script(akm, group)
        out[f"{name}_wpa1_descriptor"] = script(akm, key_frame(version, MSG1, 16, 1, anonce, bytes(8), b"", descriptor=254))
    out["frame_msg1"] = key_frame(2, MSG1, 16, 1, b"\x11" * 32, bytes(8), b"")
    out["frame_with_padding"] = key_frame(2, MSG1, 16, 1, b"\x11" * 32, bytes(8), b"") + bytes(14)
    out["frame_bad_length"] = key_frame(2, MSG1, 16, 1, b"\x11" * 32, bytes(8), b"\x01\x02")[:-1]
    out["kde_gtk"] = pad(rsn(2) + gtk_kde(1, b"\x22" * 16))
    out["empty"] = b""
    return out
