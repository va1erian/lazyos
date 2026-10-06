"""Seeds for the account and permission fuzz targets (`gen_corpus.py`):
`passwd` (the account file), `accountwire` (accounts/keyd parcels) and
`pkgstore_rules` (package manifests and permission lines). Issue #626.
"""
import struct

# ---- passwd ---------------------------------------------------------------------


def passwd_seeds():
    # `build_support/passwd` as shipped, then each way a row goes wrong.
    return {
        "shipped": b"admin:0:0:nimda:/home/admin:sh\nuser:1000:1000:lazy:/home/user:sh\n",
        "crlf_comments": b"# accounts\r\nroot:0:0:s:/root:sh\r\n\r\nbob:5:5:x:/home/bob:/bin/sh\r\n",
        "dup_uid": b"a:1:1:s:/h:sh\nb:1:1:s:/h:sh\n",
        "dup_name": b"a:1:1:s:/h:sh\na:2:1:s:/h:sh\n",
        "bad_home": b"a:1:1:s:/home/../etc:sh\n",
        "uid_overflow": b"a:4294967296:1:s:/h:sh\n",
        "short_row": b"a:1:1:s:/h\n",
        "not_utf8": b"a:1:1:\xff\xfe:/h:sh\n",
        "empty": b"",
    }


# ---- accountwire ----------------------------------------------------------------

BOOL, U32, STRING, STRUCT, OPTION = 1, 4, 7, 10, 12
ACCOUNTS_IFACE = 0x2CBF60ABBC1951BC
KEYD_IFACE = 0xD948C3355BA590BF


def _tlv(kind, field_id, payload):
    return struct.pack("<II", kind | (field_id << 8), len(payload)) + payload


def _string(field_id, text):
    return _tlv(STRING, field_id, text.encode())


def _parcel(interface, method, body):
    # version, flags, interface, method, txn, reply_to, deadline, body length,
    # handle count, buffer count (libs/messenger/src/parcel.rs).
    header = struct.pack("<HHQIQQQIHH", 1, 0, interface, method, 0, 0, 0, len(body), 0, 0)
    assert len(header) == 48
    return header + body


def accountwire_seeds():
    by_name = _tlv(OPTION, 1, _string(1, "admin")) + _tlv(OPTION, 2, b"")
    by_uid = _tlv(OPTION, 1, b"") + _tlv(OPTION, 2, _tlv(U32, 1, struct.pack("<I", 1000)))
    auth = _string(1, "user") + _string(2, "lazy")
    new_user = (_string(1, "guest") + _tlv(U32, 2, struct.pack("<I", 1001))
                + _tlv(U32, 3, struct.pack("<I", 100)) + _string(4, "pw")
                + _string(5, "/home/guest") + _string(6, "sh"))
    create = _tlv(STRUCT, 1, new_user)
    return {
        "lookup_name": _parcel(ACCOUNTS_IFACE, 1772818603, by_name),
        "lookup_uid": _parcel(ACCOUNTS_IFACE, 1772818603, by_uid),
        "authenticate": _parcel(ACCOUNTS_IFACE, 1137183084, auth),
        "create": _parcel(ACCOUNTS_IFACE, 420340861, create),
        "keyd_verify_body": auth,
        "keyd_provision": _parcel(KEYD_IFACE, 1, auth),
        "bool_reply_body": _tlv(BOOL, 1, b"\x01"),
        "truncated_tlv": by_name[:-3],
        "huge_length": struct.pack("<II", STRING | (1 << 8), 0xFFFFFFFF) + b"x",
    }


# ---- pkgstore_rules -------------------------------------------------------------

_HEADER = (
    '[app]\nname = "Demo"\nsystem_name = "org.lazy.demo"\nauthor = "A"\nversion = "1.0.0"\n'
    '[entry]\nbinary = "bin/app.elf"\n'
)


def pkgstore_rules_seeds():
    full = (
        _HEADER
        + '[permissions]\ninterfaces = ["os.lazy.confd.v1", "os.lazy.clipboard.v1"]\n'
        'topics = ["publish:app/org.lazy.demo/x", "subscribe:sys/#"]\n'
        'files = ["read:$HOME/*", "write:/tmp/demo"]\nnetwork = ["outbound"]\ndevelop = true\n'
    )
    return {
        "manifest_full": full.encode(),
        "manifest_none": (_HEADER + "[permissions]\n").encode(),
        "files_dotdot": (_HEADER + '[permissions]\nfiles = ["read:/a/../b"]\n').encode(),
        "files_globstar": (_HEADER + '[permissions]\nfiles = ["write:$HOME/**"]\n').encode(),
        "topics_globstar": (_HEADER + '[permissions]\ntopics = ["subscribe:a/**"]\n').encode(),
        "topics_dotdot": (_HEADER + '[permissions]\ntopics = ["publish:a/../b"]\n').encode(),
        "lines_mixed": b"os.lazy.confd.v1\npublish:t/1\nsubscribe:+/#\nread:$HOME/*\nwrite:/system/x\noutbound\n",
        "lines_hostile": b"publish:a/../b\nread:/a/../b\nwrite:$HOME/**\nos.lazy.nope.v9\n\xc3\xa9\n",
        "lines_many_topics": "".join(f"publish:t/{i}/{i}\n" for i in range(300)).encode(),
    }
