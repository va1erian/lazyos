"""Seeds for the `smbwire` fuzz target (`gen_corpus.py`): `libs/smbwire/src/fuzz.rs`
reads a mode byte (0 decoders, 1 framing, 2 a whole client session) and then
a server's bytes. The messages are built with the harness server's own
helpers (`tools/smb/smbproto.py`) so they are what a real server sends.
"""
import struct
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "tools" / "smb"))
import smbproto as p  # noqa: E402

SESSION = 0x0000_4400_0000_0021


def response(command, mid, status, body, tree=0):
    flags = p.FLAG_RESPONSE
    header = (p.PROTOCOL + struct.pack("<HHIHHIIQIIQ", 64, 1, status, command, 32, flags, 0, mid, 0xFEFF, tree,
                                       SESSION if command != p.NEGOTIATE else 0) + bytes(16))
    return header + body


def negotiate():
    hint = p.negotiate_hint()
    body = struct.pack("<HHHH", 65, 1, 0x0210, 0) + bytes(16) + struct.pack("<IIII", 0, 65536, 65536, 65536)
    return response(p.NEGOTIATE, 0, 0, body + struct.pack("<QQHHI", 0, 0, 128, len(hint), 0) + hint)


def challenge():
    token = p.neg_token_resp(1, p.challenge_message(bytes(range(8)), "LAZYNAS", "SERVER", 0x01DA000000000000))
    return response(p.SESSION_SETUP, 1, p.MORE_PROCESSING_REQUIRED,
                    struct.pack("<HHHH", 9, 0, 72, len(token)) + token)


def session_script():
    """What a server answers a logon, a tree connect, a listing and a read."""
    done = p.neg_token_resp(0)
    msgs = [negotiate(), challenge(),
            response(p.SESSION_SETUP, 2, 0, struct.pack("<HHHH", 9, 0, 72, len(done)) + done),
            response(p.TREE_CONNECT, 3, 0, struct.pack("<HBBIII", 16, 1, 0, 0, 0, 0x1F01FF), tree=1)]
    create = struct.pack("<HBBI", 89, 0, 0, 1) + bytes(48) + struct.pack("<II", 0x10, 0) + bytes(16) + bytes(8)
    entry = struct.pack("<II", 0, 0) + bytes(32) + struct.pack("<QQIII", 5, 8, 0x20, 2, 0) + bytes(36) + b"a\x00"
    msgs += [response(p.CREATE, 4, 0, create, tree=1),
             response(p.QUERY_DIRECTORY, 5, 0, struct.pack("<HHI", 9, 72, len(entry)) + entry, tree=1),
             response(p.QUERY_DIRECTORY, 6, p.NO_MORE_FILES, bytes([9, 0, 0, 0, 0, 0, 0, 0, 0]), tree=1),
             response(p.CLOSE, 7, 0, struct.pack("<HHI", 60, 0, 0) + bytes(52), tree=1)]
    return b"".join(p.frame(m) for m in msgs)


def smbwire_seeds():
    return {
        "decode_negotiate": b"\x00" + negotiate(),
        "decode_challenge": b"\x00" + challenge(),
        "decode_ntlm": b"\x00" + p.challenge_message(bytes(8), "D", "S", None),
        # mode % 3 == 1 selects the framing path; mode >> 2 == 1 feeds it
        # 2-byte chunks.
        "frames": b"\x04" + p.frame(negotiate()) + p.frame(challenge()),
        "session": b"\x02" + session_script(),
    }
