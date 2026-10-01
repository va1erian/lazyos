#!/usr/bin/env python3
"""Unit tests for the socket evidence checker: it must fail when it should.

Synthetic captures of a guest talking to a gateway echo server (TCP, UDP, DNS)
are built frame by frame; each check is shown to pass on a good capture and to
fail on every way the wire can be wrong: a flipped payload byte, a lost
segment, a missing FIN, a wrong checksum, a handshake that never completed, a
datagram that was not echoed, a query for the wrong name, and a host server
that saw different bytes than the capture shows.
"""

from __future__ import annotations

import struct
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import hostpeers  # noqa: E402
import pcap  # noqa: E402
import sockets_pcap as sp  # noqa: E402

GUEST = bytes.fromhex("525400123456")
GATEWAY_MAC = bytes.fromhex("525500000202")
GUEST_IP = bytes([10, 0, 2, 15])
GATEWAY_IP = bytes([10, 0, 2, 2])
DNS_IP = bytes([10, 0, 2, 3])
PORT = 47771


def checksum(data: bytes) -> int:
    return pcap.ipv4_checksum(data)


def ip_packet(src: bytes, dst: bytes, proto: int, payload: bytes) -> bytes:
    header = bytearray(struct.pack(">BBHHHBBH4s4s", 0x45, 0, 20 + len(payload), 1, 0, 64, proto, 0, src, dst))
    header[10:12] = struct.pack(">H", checksum(bytes(header)))
    return bytes(header) + payload


def with_pseudo(src: bytes, dst: bytes, proto: int, segment: bytearray, csum_at: int) -> bytes:
    segment[csum_at:csum_at + 2] = b"\0\0"
    pseudo = src + dst + struct.pack(">BBH", 0, proto, len(segment))
    segment[csum_at:csum_at + 2] = struct.pack(">H", checksum(pseudo + bytes(segment)))
    return bytes(segment)


def tcp_frame(from_guest: bool, sport: int, dport: int, seq: int, ack: int, flags: int, payload: bytes = b"",
              *, break_checksum: bool = False) -> bytes:
    src, dst = (GUEST_IP, GATEWAY_IP) if from_guest else (GATEWAY_IP, GUEST_IP)
    seg = bytearray(struct.pack(">HHIIHHHH", sport, dport, seq, ack, (5 << 12) | flags, 65535, 0, 0)) + payload
    data = with_pseudo(src, dst, 6, seg, 16)
    if break_checksum:
        data = data[:16] + bytes([data[16] ^ 0xFF]) + data[17:]
    eth = (GATEWAY_MAC + GUEST if not from_guest else GATEWAY_MAC + GUEST)
    mac_dst, mac_src = (GATEWAY_MAC, GUEST) if from_guest else (GUEST, GATEWAY_MAC)
    del eth
    return mac_dst + mac_src + struct.pack(">H", 0x0800) + ip_packet(src, dst, 6, data)


def udp_frame(from_guest: bool, sport: int, dport: int, payload: bytes, src_ip=None, dst_ip=None) -> bytes:
    src = src_ip or (GUEST_IP if from_guest else GATEWAY_IP)
    dst = dst_ip or (GATEWAY_IP if from_guest else GUEST_IP)
    seg = bytearray(struct.pack(">HHHH", sport, dport, 8 + len(payload), 0)) + payload
    data = with_pseudo(src, dst, 17, seg, 6)
    mac_dst, mac_src = (GATEWAY_MAC, GUEST) if from_guest else (GUEST, GATEWAY_MAC)
    return mac_dst + mac_src + struct.pack(">H", 0x0800) + ip_packet(src, dst, 17, data)


def echo_flow(sport: int, payload: bytes, *, fin_gateway: bool = True, fin_guest: bool = True,
              flip: int | None = None, drop_echo_tail: int = 0, handshake: bool = True,
              dport: int = PORT) -> list[bytes]:
    """One complete echo connection, in the order the wire would show it."""
    g, h = 1000, 5000
    out = []
    if handshake:
        out += [tcp_frame(True, sport, dport, g, 0, sp.SYN),
                tcp_frame(False, dport, sport, h, g + 1, sp.SYN | sp.ACK),
                tcp_frame(True, sport, dport, g + 1, h + 1, sp.ACK)]
    else:
        out += [tcp_frame(True, sport, dport, g, 0, sp.SYN)]
    gseq, hseq = g + 1, h + 1
    echoed = bytearray(payload)
    if flip is not None:
        echoed[flip] ^= 0x01
    if drop_echo_tail:
        echoed = echoed[:-drop_echo_tail]
    for at in range(0, len(payload), 1400):
        chunk = payload[at:at + 1400]
        out.append(tcp_frame(True, sport, dport, gseq, hseq, sp.ACK | sp.PSH, chunk))
        gseq += len(chunk)
        back = bytes(echoed[at:at + 1400])
        if back:
            out.append(tcp_frame(False, dport, sport, hseq, gseq, sp.ACK | sp.PSH, back))
            hseq += len(back)
    if fin_guest:
        out.append(tcp_frame(True, sport, dport, gseq, hseq, sp.FIN | sp.ACK))
        gseq += 1
    if fin_gateway:
        out.append(tcp_frame(False, dport, sport, hseq, gseq, sp.FIN | sp.ACK))
        hseq += 1
    out.append(tcp_frame(True, sport, dport, gseq, hseq, sp.ACK))
    return out


def frames_of(raw: list[bytes]) -> list[pcap.Frame]:
    return pcap.read_pcap(pcap.write_pcap(raw))


def dns_query(name: str, txid: int = 0x1234, qtype: int = 1) -> bytes:
    labels = b"".join(bytes([len(p)]) + p.encode() for p in name.split("."))
    return struct.pack(">HHHHHH", txid, 0x0100, 1, 0, 0, 0) + labels + b"\0" + struct.pack(">HH", qtype, 1)


def check_flows(raw, servers, min_flows=1):
    return sp.check_echo_flows(frames_of(raw), GUEST_IP, GATEWAY_IP, PORT, min_flows, servers)


class TcpFlows(unittest.TestCase):
    PAYLOAD = hostpeers.pattern(4000)

    def test_a_good_echo_passes(self):
        raw = echo_flow(50000, self.PAYLOAD) + echo_flow(50001, b"hello\n")
        count, problems = check_flows(raw, [self.PAYLOAD, b"hello\n"], 2)
        self.assertEqual((count, problems), (2, []))

    def test_a_flipped_echo_byte_fails(self):
        _, problems = check_flows(echo_flow(50000, self.PAYLOAD, flip=2500), [self.PAYLOAD])
        self.assertTrue(any("differ" in p for p in problems), problems)

    def test_a_short_echo_fails(self):
        _, problems = check_flows(echo_flow(50000, self.PAYLOAD, drop_echo_tail=10), [self.PAYLOAD])
        self.assertTrue(any("lengths differ" in p for p in problems), problems)

    def test_a_missing_guest_fin_fails(self):
        _, problems = check_flows(echo_flow(50000, self.PAYLOAD, fin_guest=False), [self.PAYLOAD])
        self.assertTrue(any("FIN" in p for p in problems), problems)

    def test_a_missing_gateway_fin_fails(self):
        _, problems = check_flows(echo_flow(50000, self.PAYLOAD, fin_gateway=False), [self.PAYLOAD])
        self.assertTrue(any("FIN" in p for p in problems), problems)

    def test_no_handshake_fails(self):
        _, problems = check_flows(echo_flow(50000, self.PAYLOAD, handshake=False), [self.PAYLOAD])
        self.assertTrue(any("handshake" in p for p in problems), problems)

    def test_too_few_flows_fails(self):
        _, problems = check_flows(echo_flow(50000, b"x"), [b"x"], min_flows=2)
        self.assertTrue(any("2 required" in p for p in problems), problems)

    def test_a_lost_segment_is_a_gap(self):
        raw = echo_flow(50000, self.PAYLOAD)
        data_frames = [i for i, f in enumerate(raw) if len(f) > 1000 and f[6:12] == GUEST]
        del raw[data_frames[1]]
        _, problems = check_flows(raw, [self.PAYLOAD])
        self.assertTrue(any("gap" in p for p in problems), problems)

    def test_a_retransmission_is_fine(self):
        raw = echo_flow(50000, self.PAYLOAD)
        first = next(i for i, f in enumerate(raw) if len(f) > 1000 and f[6:12] == GUEST)
        raw.insert(first + 1, raw[first])
        count, problems = check_flows(raw, [self.PAYLOAD])
        self.assertEqual((count, problems), (1, []))

    def test_the_host_server_seeing_other_bytes_fails(self):
        _, problems = check_flows(echo_flow(50000, self.PAYLOAD), [self.PAYLOAD[:-1] + b"?"])
        self.assertTrue(any("do not match what the host server" in p for p in problems), problems)

    def test_the_host_server_seeing_nothing_fails(self):
        _, problems = check_flows(echo_flow(50000, self.PAYLOAD), [])
        self.assertTrue(any("do not match" in p for p in problems), problems)

    def test_a_reset_flow_fails(self):
        raw = echo_flow(50000, b"x")
        raw.append(tcp_frame(False, PORT, 50000, 9999, 0, sp.RST))
        _, problems = check_flows(raw, [b"x"])
        self.assertTrue(any("reset" in p for p in problems), problems)

    def test_flows_to_another_port_are_not_counted(self):
        _, problems = check_flows(echo_flow(50000, b"x", dport=PORT + 1), [], min_flows=1)
        self.assertTrue(any("0 TCP flows" in p for p in problems), problems)

    def test_a_reused_port_is_a_new_flow(self):
        raw = echo_flow(50000, b"one") + echo_flow(50000, b"two")
        count, problems = check_flows(raw, [b"one", b"two"], 2)
        self.assertEqual((count, problems), (2, []))


class Checksums(unittest.TestCase):
    def test_good_segments_pass(self):
        self.assertEqual(sp.check_checksums(frames_of(echo_flow(50000, b"abc")), GUEST), [])

    def test_a_wrong_tcp_checksum_fails(self):
        raw = [tcp_frame(True, 50000, PORT, 1, 0, sp.SYN, break_checksum=True)]
        self.assertTrue(sp.check_checksums(frames_of(raw), GUEST))

    def test_a_wrong_udp_checksum_fails(self):
        frame = bytearray(udp_frame(True, 50000, 47772, b"abc"))
        frame[14 + 20 + 6] ^= 0x55
        self.assertTrue(sp.check_checksums(frames_of([bytes(frame)]), GUEST))

    def test_the_gateways_frames_are_not_judged(self):
        raw = [tcp_frame(False, PORT, 50000, 1, 0, sp.SYN | sp.ACK, break_checksum=True)]
        self.assertEqual(sp.check_checksums(frames_of(raw), GUEST), [])


class Refusals(unittest.TestCase):
    def syn_rst(self, sport):
        return [tcp_frame(True, sport, 47999, 1, 0, sp.SYN), tcp_frame(False, 47999, sport, 0, 2, sp.RST | sp.ACK)]

    def test_refused_connections_are_counted(self):
        raw = self.syn_rst(50000) + self.syn_rst(50001)
        count, problems = sp.check_refused(frames_of(raw), GUEST_IP, GATEWAY_IP, 47999, 2)
        self.assertEqual((count, problems), (2, []))

    def test_silence_is_an_acceptable_answer_but_still_an_attempt(self):
        raw = [tcp_frame(True, 50000, 47999, 1, 0, sp.SYN)]
        count, problems = sp.check_refused(frames_of(raw), GUEST_IP, GATEWAY_IP, 47999, 1)
        self.assertEqual((count, problems), (0, []))

    def test_too_few_attempts_fail(self):
        count, problems = sp.check_refused(frames_of([]), GUEST_IP, GATEWAY_IP, 47999, 1)
        self.assertEqual(count, 0)
        self.assertTrue(any("1 required" in p for p in problems), problems)

    def test_an_established_connection_to_the_closed_port_fails(self):
        raw = echo_flow(50000, b"x", dport=47999)
        _, problems = sp.check_refused(frames_of(raw), GUEST_IP, GATEWAY_IP, 47999, 1)
        self.assertTrue(any("was established" in p for p in problems), problems)


class UdpEcho(unittest.TestCase):
    def pair(self, sport, payload, reply=None):
        return [udp_frame(True, sport, 47772, payload), udp_frame(False, 47772, sport, payload if reply is None else reply)]

    def test_good_pairs_pass(self):
        raw = self.pair(50000, b"a") + self.pair(50001, b"bb")
        count, problems = sp.check_udp_echo(frames_of(raw), GUEST_IP, GATEWAY_IP, 47772, 2, [b"a", b"bb"])
        self.assertEqual((count, problems), (2, []))

    def test_a_different_reply_fails(self):
        raw = self.pair(50000, b"a", reply=b"b")
        _, problems = sp.check_udp_echo(frames_of(raw), GUEST_IP, GATEWAY_IP, 47772, 1, [b"a"])
        self.assertTrue(any("not echoed" in p for p in problems), problems)

    def test_no_reply_fails(self):
        raw = [udp_frame(True, 50000, 47772, b"a")]
        _, problems = sp.check_udp_echo(frames_of(raw), GUEST_IP, GATEWAY_IP, 47772, 1, [b"a"])
        self.assertTrue(problems)

    def test_the_server_seeing_other_datagrams_fails(self):
        raw = self.pair(50000, b"a")
        _, problems = sp.check_udp_echo(frames_of(raw), GUEST_IP, GATEWAY_IP, 47772, 1, [b"z"])
        self.assertTrue(any("do not match" in p for p in problems), problems)


class Dns(unittest.TestCase):
    def query_frame(self, name="localhost", sport=50053, **kw):
        return udp_frame(True, sport, 53, dns_query(name, **kw), dst_ip=DNS_IP)

    def test_a_query_without_an_answer_is_reported_not_failed(self):
        detail, problems = sp.check_dns(frames_of([self.query_frame()]), GUEST_IP, "localhost")
        self.assertEqual(problems, [])
        self.assertIn("no answer", detail)

    def test_an_answer_is_reported(self):
        answer = bytearray(dns_query("localhost"))
        answer[2:4] = struct.pack(">H", 0x8183)
        raw = [self.query_frame(), udp_frame(False, 53, 50053, bytes(answer), src_ip=DNS_IP)]
        detail, problems = sp.check_dns(frames_of(raw), GUEST_IP, "localhost")
        self.assertEqual(problems, [])
        self.assertIn("rcode=3", detail)

    def test_no_query_fails(self):
        _, problems = sp.check_dns(frames_of([]), GUEST_IP, "localhost")
        self.assertTrue(problems)

    def test_a_query_for_another_name_fails(self):
        _, problems = sp.check_dns(frames_of([self.query_frame("example.test")]), GUEST_IP, "localhost")
        self.assertTrue(problems)

    def test_a_query_of_another_type_fails(self):
        _, problems = sp.check_dns(frames_of([self.query_frame(qtype=28)]), GUEST_IP, "localhost")
        self.assertTrue(any("not a well-formed A" in p for p in problems), problems)

    def test_the_name_parser_refuses_garbage(self):
        for message in (b"", b"\0" * 12, dns_query("a.b")[:15], b"\x12\x34\x01\x00\x00\x01" + b"\0" * 6 + b"\xff" + b"x" * 10):
            self.assertIsNone(sp.dns_name(message), message)


class Inbound(unittest.TestCase):
    PAYLOAD = hostpeers.pattern(3000)

    def flow(self, incoming, outgoing, **kw):
        """The harness (as the gateway) connecting to the guest's port 47773."""
        g, h = 7000, 9000
        raw = [tcp_frame(False, 40000, 47773, g, 0, sp.SYN),
               tcp_frame(True, 47773, 40000, h, g + 1, sp.SYN | sp.ACK),
               tcp_frame(False, 40000, 47773, g + 1, h + 1, sp.ACK)]
        gseq, hseq = g + 1, h + 1
        for at in range(0, max(len(incoming), len(outgoing)), 1400):
            part = incoming[at:at + 1400]
            if part:
                raw.append(tcp_frame(False, 40000, 47773, gseq, hseq, sp.ACK, part))
                gseq += len(part)
            back = outgoing[at:at + 1400]
            if back:
                raw.append(tcp_frame(True, 47773, 40000, hseq, gseq, sp.ACK, back))
                hseq += len(back)
        return raw

    def check(self, raw):
        return sp.check_inbound_flow(frames_of(raw), GUEST_IP, GATEWAY_IP, 47773, self.PAYLOAD)

    def test_a_good_inbound_flow_passes(self):
        count, problems = self.check(self.flow(self.PAYLOAD, self.PAYLOAD))
        self.assertEqual((count, problems), (len(self.PAYLOAD), []))

    def test_an_echo_that_differs_fails(self):
        wrong = self.PAYLOAD[:-1] + b"!"
        _, problems = self.check(self.flow(self.PAYLOAD, wrong))
        self.assertTrue(any("echoed" in p for p in problems), problems)

    def test_a_short_receive_fails(self):
        _, problems = self.check(self.flow(self.PAYLOAD[:-5], self.PAYLOAD))
        self.assertTrue(any("received" in p for p in problems), problems)

    def test_no_connection_fails(self):
        _, problems = self.check([])
        self.assertTrue(any("no connection" in p for p in problems), problems)


def stream_flow(sport: int, dport: int, up: bytes, down: bytes, *, fin_gateway: bool = True,
                fin_guest: bool = True) -> list[bytes]:
    """A connection carrying `up` (guest to gateway) and then `down` bytes."""
    g, h = 1000, 5000
    out = [tcp_frame(True, sport, dport, g, 0, sp.SYN),
           tcp_frame(False, dport, sport, h, g + 1, sp.SYN | sp.ACK),
           tcp_frame(True, sport, dport, g + 1, h + 1, sp.ACK)]
    gseq, hseq = g + 1, h + 1
    for at in range(0, len(up), 1400):
        part = up[at:at + 1400]
        out.append(tcp_frame(True, sport, dport, gseq, hseq, sp.ACK | sp.PSH, part))
        gseq += len(part)
    for at in range(0, len(down), 1400):
        part = down[at:at + 1400]
        out.append(tcp_frame(False, dport, sport, hseq, gseq, sp.ACK | sp.PSH, part))
        hseq += len(part)
    if fin_guest:
        out.append(tcp_frame(True, sport, dport, gseq, hseq, sp.FIN | sp.ACK))
        gseq += 1
    if fin_gateway:
        out.append(tcp_frame(False, dport, sport, hseq, gseq, sp.FIN | sp.ACK))
    return out


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
