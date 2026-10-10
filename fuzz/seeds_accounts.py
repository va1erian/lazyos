"""Seeds for the account and permission fuzz targets (`gen_corpus.py`):
`passwd` (the account file), `accountwire` (accounts/keyd parcels) and
`pkgstore_rules` (package manifests and permission lines). Issue #626.
"""
import struct

# ---- passwd ---------------------------------------------------------------------


def passwd_seeds():
    # `build_support/passwd` as shipped (the password field is `x`: the
    # verifiers are in /system/etc/shadow, #447), then each way a row goes wrong.
    return {
        "shipped": b"admin:0:0:x:/home/admin:sh\nuser:1000:1000:x:/home/user:sh\n",
        "crlf_comments": b"# accounts\r\nroot:0:0:x:/root:sh\r\n\r\nbob:5:5:x:/home/bob:/bin/sh\r\n",
        "dup_uid": b"a:1:1:x:/h:sh\nb:1:1:x:/h:sh\n",
        "dup_name": b"a:1:1:x:/h:sh\na:2:1:x:/h:sh\n",
        "bad_home": b"a:1:1:x:/home/../etc:sh\n",
        "uid_overflow": b"a:4294967296:1:x:/h:sh\n",
        "short_row": b"a:1:1:x:/h\n",
        "not_utf8": b"a:1:1:\xff\xfe:/h:sh\n",
        "plaintext": b"admin:0:0:nimda:/home/admin:sh\n",
        "empty": b"",
    }


# ---- accountdb ------------------------------------------------------------------

_VERIFIER = "argon2id:19456:2:1:" + "ab" * 16 + ":" + "5a" * 32


def accountdb_seeds():
    # The image's seed database (docs/accounts-plan.md U1), the setup state,
    # then each way a record goes wrong.
    seed = ("# seed\nnext:1002\ngroup:admin:10\n"
            f"user:user:1000:1000:/home/user:sh::{_VERIFIER}\n"
            f"user:admin:1001:1001:/home/admin:sh:admin:{_VERIFIER}\n")
    return {
        "seed": seed.encode(),
        "setup": b"group:admin:10\n",
        "empty": b"",
        "locked": b"user:bob:1002:1002:/home/bob:sh::!\r\n",
        "uid_zero": b"user:root:0:0:/root:sh::!\n",
        "unknown_group": b"user:bob:1002:1002:/home/bob:sh:wheel:!\n",
        "dup_gid": b"group:admin:10\ngroup:wheel:10\n",
        "bad_secret": b"user:bob:1002:1002:/home/bob:sh::argon2id:1:1:1:ab:cd\n",
        "bad_next": b"next:12\n",
        "plaintext": b"user:admin:1001:1001:/home/admin:sh:admin:nimda\n",
    }


# ---- elevpolicy ------------------------------------------------------------------


def elevpolicy_seeds():
    # One request per row of elevd's operation table (docs/accounts-plan.md
    # U2), NUL-separated, then the hostile shapes.
    def request(*parts):
        return "\0".join(parts).encode()
    return {
        "pkg_install": request("pkg.install", "/transient/demo.lzp"),
        "pkg_update_core": request("pkg.update-core", "/home/user/counter.lzp"),
        "pkg_remove": request("pkg.remove", "org.lazy.demo"),
        "conf_set": request("conf.set", "sys/ui/demo", "str", "hello"),
        "conf_set_bytes": request("conf.set", "sys/ui/demo", "bytes", "00ff"),
        "conf_list": request("conf.list", ""),
        "conf_elevate": request("conf.elevate"),
        "time_set": request("time.set", "1767225600"),
        "account_create": request("account.create", "bob", "s3cret", "admin"),
        "account_delete": request("account.delete", "bob", "archive"),
        "account_admin": request("account.admin", "user", "1"),
        "account_password": request("account.password", "admin", "nimda"),
        "power_policy": request("power.policy", "button", "shutdown"),
        "service_restart": request("service.restart", "inputd"),
        "wifi_system_store": request("net.wifi.system", "store", "office", "correct horse"),
        "wifi_system_delete": request("net.wifi.system", "delete", "office"),
        "unknown": request("sh", "-c", "rm -rf /"),
        "traversal": request("pkg.install", "/transient/../system/bin/init"),
        "system_account": request("account.create", "_accounts", "x", "admin"),
        # Review of #659: text that would mislead the prompt or forge an
        # audit line, a value that would not fit, a guarded service.
        "conf_set_newline": request("conf.set", "sys/ui/demo", "str",
                                    "x\nELEVD:REQUEST op=account.admin outcome=granted"),
        "conf_set_bidi": request("conf.set", "sys/ui/demo", "str", "abc\u202efed"),
        "conf_set_quotes": request("conf.set", "sys/ui/demo", "str",
                                   'a" outcome=granted "' + " " * 200 + "b"),
        "restart_elevd": request("service.restart", "elevd"),
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
