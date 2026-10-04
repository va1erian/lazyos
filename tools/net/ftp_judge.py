"""Stage N4's verdict on the FTP session (split out of `run.py`, file-size budget):
the session on the wire (`sockets_pcap.check_ftp`) and the checksums the client
printed against the bytes the host's server holds."""

from __future__ import annotations

import re
import zlib

import analyze_pcap
import hostpeers
import pcap
import sockets_pcap

#: The files the server starts with, what `put -g` sends, and the commands the
#: client is expected to issue (the capture must show exactly the server's
#: record, and this is what that record must be).
FTP_UPLOAD_BYTES = 150_000
FTP_FILES = {
    "hello.txt": b"hello from the host ftp server" + bytes([10]),
    "big.bin": hostpeers.pattern(120_000),
}
FTP_EXPECTED_VERBS = ["USER", "PASS", "TYPE", "PWD", "CWD", "CWD", "PASV", "LIST", "PASV", "RETR", "PASV",
                      "RETR", "PASV", "STOR", "SIZE", "PASV", "RETR", "QUIT"]


def judge_ftp(frames, guest_ip: bytes, text: str, commands, transfers) -> bool:
    """Stage N4's verdict: the FTP session on the wire (`sockets_pcap.check_ftp`)
    and the checksums the client printed against the bytes the server holds."""
    gateway = pcap.parse_ip(analyze_pcap.DEFAULT_GATEWAY)
    problems: list[str] = []
    verbs = [v for v, _ in commands]
    if verbs != FTP_EXPECTED_VERBS:
        problems.append(f"the server saw the commands {verbs}, expected {FTP_EXPECTED_VERBS}")
    count, wire_problems = sockets_pcap.check_ftp(frames, guest_ip, gateway, hostpeers.FTP_PORT, commands, transfers)
    problems += wire_problems
    upload = hostpeers.xorshift_pattern(FTP_UPLOAD_BYTES)
    expected = {
        "PUT up.bin": (FTP_UPLOAD_BYTES, upload),
        "GET up.bin": (FTP_UPLOAD_BYTES, upload),
    }
    for name, body in FTP_FILES.items():
        expected[f"GET {name}"] = (len(body), body)
    for key, (size, body) in expected.items():
        verb, name = key.split()
        match = re.search(rf"FTP:{verb} {re.escape(name)} bytes=(\d+) crc=([0-9a-f]{{8}})", text)
        if not match:
            problems.append(f"the client never reported {key}")
        elif (int(match.group(1)), match.group(2)) != (size, f"{zlib.crc32(body):08x}"):
            problems.append(f"{key}: the client reported {match.group(1)} bytes crc {match.group(2)}, "
                            f"expected {size} bytes crc {zlib.crc32(body):08x}")
    if not any(t[1] == "up" and t[2] == upload for t in transfers):
        problems.append("the server never received the uploaded stream")
    if problems:
        for problem in problems[:10]:
            print(f"NET:PCAP:FTP:FAIL {problem}")
        return False
    print(f"NET:PCAP:FTP:PASS commands={len(commands)} transfers={count} download and upload match byte for byte")
    return True
