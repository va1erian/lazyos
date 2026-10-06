#!/usr/bin/env python3
"""Unit tests for the socket checker's FTP session judge and the host peers
(echo servers, FTP server, patterns); `test_sockets_pcap.py` runs them too.
"""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import hostpeers  # noqa: E402
import sockets_pcap as sp  # noqa: E402
from sockets_fixtures import GATEWAY_IP, GUEST_IP, frames_of, stream_flow  # noqa: E402


class Ftp(unittest.TestCase):
    CTRL = 47780
    FILE = hostpeers.xorshift_pattern(3000)
    UPLOAD = hostpeers.xorshift_pattern(2500)
    COMMANDS = [("USER", "lazy"), ("PASS", "os"), ("PASV", ""), ("RETR", "a.bin"), ("PASV", ""),
                ("STOR", "up.bin"), ("QUIT", "")]
    TRANSFERS = [(41000, "down", FILE), (41001, "up", UPLOAD)]

    def capture(self, *, commands=None, down=None, up=None, ctrl_fin=True, extra=()):
        wire = "".join(f"{v} {a}".rstrip() + "\r\n" for v, a in (commands or self.COMMANDS)).encode()
        reply = b"220 hi\r\n331 x\r\n230 y\r\n227 z\r\n150 a\r\n226 b\r\n"
        raw = stream_flow(50000, self.CTRL, wire, reply, fin_guest=ctrl_fin)
        raw += stream_flow(50001, 41000, b"", self.FILE if down is None else down)
        raw += stream_flow(50002, 41001, self.UPLOAD if up is None else up, b"")
        for sport, dport in extra:
            raw += stream_flow(sport, dport, b"x", b"")
        return frames_of(raw)

    def check(self, frames, commands=None, transfers=None):
        return sp.check_ftp(frames, GUEST_IP, GATEWAY_IP, self.CTRL, commands or self.COMMANDS,
                            transfers or self.TRANSFERS)

    def test_a_good_session_passes(self):
        count, problems = self.check(self.capture())
        self.assertEqual((count, problems), (2, []))

    def test_a_flipped_download_byte_fails(self):
        bad = bytearray(self.FILE)
        bad[1000] ^= 1
        _, problems = self.check(self.capture(down=bytes(bad)))
        self.assertTrue(any("differ" in p for p in problems), problems)

    def test_a_short_upload_fails(self):
        _, problems = self.check(self.capture(up=self.UPLOAD[:-1]))
        self.assertTrue(any("2499 bytes on the wire" in p for p in problems), problems)

    def test_a_command_the_server_never_saw_fails(self):
        injected = self.COMMANDS[:-1] + [("DELE", "x"), ("QUIT", "")]
        _, problems = self.check(self.capture(commands=injected))
        self.assertTrue(any("differ from what the server recorded" in p for p in problems), problems)

    def test_a_missing_control_fin_fails(self):
        _, problems = self.check(self.capture(ctrl_fin=False))
        self.assertTrue(any("control connection was not closed" in p for p in problems), problems)

    def test_a_transfer_with_no_connection_fails(self):
        _, problems = self.check(self.capture(), transfers=self.TRANSFERS + [(41002, "down", b"zz")])
        self.assertTrue(any("0 connections to its port" in p for p in problems), problems)

    def test_a_connection_to_an_unrelated_port_fails(self):
        _, problems = self.check(self.capture(extra=[(50003, 22222)]))
        self.assertTrue(any("not part of the FTP session" in p for p in problems), problems)

    def test_bytes_the_wrong_way_fail(self):
        raw = (stream_flow(50000, self.CTRL, b"QUIT\r\n", b"221 bye\r\n")
               + stream_flow(50001, 41000, b"unexpected", self.FILE))
        _, problems = sp.check_ftp(frames_of(raw), GUEST_IP, GATEWAY_IP, self.CTRL, [("QUIT", "")],
                                   [(41000, "down", self.FILE)])
        self.assertTrue(any("flowed the wrong way" in p for p in problems), problems)

    def test_no_control_connection_fails(self):
        count, problems = self.check(frames_of([]))
        self.assertEqual(count, 0)
        self.assertTrue(problems)

    def test_the_command_parser(self):
        self.assertEqual(sp.ftp_commands(b"USER a\r\nPWD\r\n"), [("USER", "a"), ("PWD", "")])
        self.assertEqual(sp.ftp_commands(b""), [])


class Peers(unittest.TestCase):
    def test_the_python_pattern_matches_the_rust_one(self):
        self.assertEqual(list(hostpeers.xorshift_pattern(16)),
                         [11, 2, 229, 54, 161, 78, 214, 26, 176, 73, 184, 86, 173, 214, 63, 252])

    def test_the_ftp_server_serves_and_records(self):
        import ftplib
        body = hostpeers.xorshift_pattern(5000)
        server = hostpeers.FtpServer(port=0, files={"a.bin": body})
        try:
            port = server._listener.getsockname()[1]
            ftp = ftplib.FTP()
            ftp.connect("127.0.0.1", port, timeout=10)
            ftp.login(hostpeers.FTP_USER, hostpeers.FTP_PASS)
            # The server reports the guest-side address; connect to the loopback instead.
            ftp.set_pasv(True)
            ftp.makepasv = self._pasv(ftp)
            got = bytearray()
            ftp.retrbinary("RETR a.bin", got.extend)
            self.assertEqual(bytes(got), body)
            import io
            ftp.storbinary("STOR up.bin", io.BytesIO(b"hello"))
            self.assertEqual(ftp.size("up.bin"), 5)
            with self.assertRaises(ftplib.error_perm):
                ftp.retrbinary("RETR ../etc", lambda b: None)
            ftp.quit()
            self.assertEqual(server.uploads["up.bin"], b"hello")
            self.assertEqual([(d, len(b)) for _, d, b in server.transfers], [("down", 5000), ("up", 5)])
            verbs = [v for v, _ in server.commands]
            self.assertEqual(verbs[:2], ["USER", "PASS"])
            self.assertEqual(verbs[-1], "QUIT")
        finally:
            server.close()

    @staticmethod
    def _pasv(ftp):
        def makepasv():
            reply = ftp.sendcmd("PASV")
            fields = reply.split("(")[1].split(")")[0].split(",")
            return "127.0.0.1", int(fields[4]) * 256 + int(fields[5])
        return makepasv

    def test_the_pattern_is_deterministic_and_varied(self):
        a, b = hostpeers.pattern(1000), hostpeers.pattern(1000)
        self.assertEqual(a, b)
        self.assertGreater(len(set(a)), 200)

    def test_the_echo_servers_record_and_echo(self):
        import socket
        servers = hostpeers.EchoServers(tcp_port=0, udp_port=0)
        try:
            tcp_port = servers._tcp.getsockname()[1]
            udp_port = servers._udp.getsockname()[1]
            with socket.create_connection(("127.0.0.1", tcp_port), timeout=5) as conn:
                conn.sendall(b"abc" * 1000)
                got = b""
                while len(got) < 3000:
                    got += conn.recv(4096)
                conn.shutdown(socket.SHUT_WR)
                self.assertEqual(got, b"abc" * 1000)
            with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as udp:
                udp.settimeout(5)
                udp.sendto(b"ping", ("127.0.0.1", udp_port))
                self.assertEqual(udp.recvfrom(100)[0], b"ping")
            import time
            for _ in range(50):
                streams, datagrams = servers.snapshot()
                if streams and datagrams:
                    break
                time.sleep(0.05)
            self.assertEqual(streams, [b"abc" * 1000])
            self.assertEqual(datagrams, [b"ping"])
        finally:
            servers.close()



if __name__ == "__main__":
    unittest.main()
