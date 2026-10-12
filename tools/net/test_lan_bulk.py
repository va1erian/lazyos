#!/usr/bin/env python3
"""Tests of the LAN throughput tool's parsing and cross-check
(`tools/net/lan_bulk.py`): they fail when the judge should.

    python tools/net/test_lan_bulk.py

The topic payloads are the ones a real AMD NUC (RTL8168 x2, RTL8822CE Wi-Fi
joined to a 5 GHz network) returned over `dbgd`.
"""

from __future__ import annotations

import hashlib
import json
import socket
import sys
import threading
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(HERE.parent / "dbg"))
import lan_bulk_judge as judge  # noqa: E402
import lanbox  # noqa: E402
from bulkpeers import BulkServer, Transfer  # noqa: E402
from dbgclient import DbgClient, PROTO, client_mac, server_mac  # noqa: E402

INTERFACES_HEX = (
    "02000000000000006f732e636e74726c0100000061010000000000000000000000000000000000000000000000000000"
    "080200005901000009010000510100000a0100006800000007010000040000006574683008020000060000008447098dc8"
    "0f0403000004000000dc0500000104000001000000000405000004000000000000000406000004000000010000000407"
    "000004000000000000000408000004000000640000000109000001000000000a010000680000000701000004000000657468"
    "3108020000060000008447098dc80e0403000004000000dc05000001040000010000000004050000040000000000000004"
    "0600000400000001000000040700000400000000000000040800000400000064000000010900000100000000"
    "0a010000690000000701000005000000776c616e3008020000060000000ccdb4c108200403000004000000dc0500000104"
    "00000100000001040500000400000000000000040600000400000002000000040700000400000001000000040800000400"
    "000058020000010900000100000001")
WLAN_ADDR_HEX = (
    "02000000000000006f732e636e74726c010000003900000000000000000000000000000000000000000000000000000008"
    "020000310000000701000005000000776c616e300802000004000000c0a801510403000004000000180000000804000004"
    "000000c0a801fe")
WLAN_STATE_HEX = (
    "02000000000000006f732e636e74726c010000006b00000000000000000000000000000000000000000000000000000008"
    "020000630000000401000004000000060000000702000009000000436861746f6e4e455408030000060000006a15241645"
    "0c0204000004000000eaffffff04050000040000007c1500000706000000000000040700000400000000000000040800000400000005000000")


#: A QEMU guest's eth0 address (10.0.2.15): the value arrives as a String field.
QEMU_ADDR_HEX = (
    "02000000000000006f732e636e74726c0100000038000000000000000000000000000000000000000000000000000000"
    "070100003000000007010000040000006574683008020000040000000a00020f04030000040000001800000008040000"
    "040000000a000202")


def snapshot(wired_primary: bool = False, wired_up: bool = False) -> lanbox.BoxSnapshot:
    """The NUC's interfaces, optionally with the cable in."""
    eth = lanbox.Iface("eth0", "84:47:09:8d:c8:0f", wired_up, "wired", "bound", 100, wired_primary,
                       "192.168.1.60" if wired_up else "0.0.0.0", 24, "192.168.1.254" if wired_up else "0.0.0.0")
    wlan = lanbox.Iface("wlan0", "0c:cd:b4:c1:08:20", True, "wireless", "bound", 600, not wired_primary,
                        "192.168.1.81", 24, "192.168.1.254",
                        lanbox.WifiLink("connected", "ChatonNET", "", -22, 5500, "5 GHz", 100))
    return lanbox.BoxSnapshot(1000, [eth, wlan], 1234)


def guest(kind: str, ms: int, size: int, pos: int = 0, verdict: str = "PASS") -> lanbox.GuestResult:
    mbps = round(size / ms / 1000, 2)
    return lanbox.GuestResult(pos, "native", kind, verdict, size, ms, mbps, "" if verdict == "PASS" else "boom")


def transfer(kind: str, seed: int, size: int, seconds: float, peer: str = "192.168.1.81",
             ok: bool = True, received: int | None = None) -> Transfer:
    return Transfer(kind, size, seed, received=size if received is None else received, ok=ok,
                    problem="" if ok else "byte 5 differs", seconds=seconds, peer=peer, done=True)


class TopicDecoding(unittest.TestCase):
    def test_interfaces(self):
        value = lanbox.topic_value({"payload_hex": INTERFACES_HEX}, lanbox.INTERFACE_LIST)
        names = [i["name"] for i in value["list"]]
        self.assertEqual(names, ["eth0", "eth1", "wlan0"])
        wlan = value["list"][2]
        self.assertEqual((wlan["kind"], wlan["metric"], wlan["link"], wlan["primary"]), (1, 600, True, True))
        self.assertEqual(wlan["mac"], "0c:cd:b4:c1:08:20")
        self.assertFalse(value["list"][0]["link"])
        self.assertEqual(value["list"][0]["metric"], 100)

    def test_address(self):
        value = lanbox.topic_value({"payload_hex": WLAN_ADDR_HEX}, lanbox.ADDRESS)
        self.assertEqual((value["interface"], value["addr"], value["prefix"], value["gateway"]),
                         ("wlan0", "192.168.1.81", 24, "192.168.1.254"))

    def test_address_wrapped_as_a_string(self):
        value = lanbox.topic_value({"payload_hex": QEMU_ADDR_HEX}, lanbox.ADDRESS)
        self.assertEqual((value["interface"], value["addr"], value["prefix"], value["gateway"]),
                         ("eth0", "10.0.2.15", 24, "10.0.2.2"))

    def test_wifi_state(self):
        link = lanbox.WifiLink.from_topic(lanbox.topic_value({"payload_hex": WLAN_STATE_HEX}, lanbox.WIFI_STATE))
        self.assertEqual((link.state, link.ssid, link.rssi_dbm, link.freq_mhz), ("connected", "ChatonNET", -22, 5500))
        self.assertEqual((link.band, link.channel), ("5 GHz", 100))
        self.assertIn("rssi -22 dBm", link.describe())

    def test_truncated_payloads_are_refused(self):
        for bad in (INTERFACES_HEX[:40], INTERFACES_HEX[:-30], ""):
            with self.assertRaises(lanbox.TlvError):
                lanbox.topic_value({"payload_hex": bad}, lanbox.INTERFACE_LIST)

    def test_channels(self):
        self.assertEqual(lanbox.channel_of(2412), ("2.4 GHz", 1))
        self.assertEqual(lanbox.channel_of(2462), ("2.4 GHz", 11))
        self.assertEqual(lanbox.channel_of(2484), ("2.4 GHz", 14))
        self.assertEqual(lanbox.channel_of(5180), ("5 GHz", 36))
        self.assertEqual(lanbox.channel_of(5955), ("6 GHz", 1))
        self.assertEqual(lanbox.channel_of(0), ("", 0))


class RouteCheck(unittest.TestCase):
    def test_wifi_alone_passes(self):
        self.assertEqual(lanbox.check_route(snapshot(), "wlan0"), ([], []))

    def test_cable_in_fails_with_unplug_hint(self):
        problems, hints = lanbox.check_route(snapshot(wired_primary=True, wired_up=True), "wlan0")
        self.assertTrue(any("default route" in p and "eth0" in p for p in problems))
        self.assertTrue(any("unplug" in h for h in hints))

    def test_wired_under_test_while_wifi_primary_fails(self):
        problems, _ = lanbox.check_route(snapshot(), "eth0")
        self.assertTrue(any("link is down" in p for p in problems))
        self.assertTrue(any("no IPv4" in p for p in problems))

    def test_unknown_interface(self):
        problems, _ = lanbox.check_route(snapshot(), "wlan9")
        self.assertIn("no interface wlan9", problems[0])


class GuestLines(unittest.TestCase):
    def test_pass_fail_done(self):
        r = lanbox.parse_guest_line("NETBULK:native:PUT:PASS bytes=16777216 ms=6230 mbps=2.69", 7)
        self.assertEqual((r.path, r.kind, r.verdict, r.bytes, r.ms, r.mbps, r.pos),
                         ("native", "PUT", "PASS", 16777216, 6230, 2.69, 7))
        r = lanbox.parse_guest_line("NETBULK:native:GET:FAIL:receive timed out")
        self.assertEqual((r.kind, r.verdict), ("GET", "FAIL"))
        self.assertIn("timed out", r.detail)
        d = lanbox.parse_guest_line("NETBULK:native:done ok=false")
        self.assertFalse(d.ok)
        self.assertTrue(lanbox.parse_guest_line("NETBULK:native:done ok=true").ok)

    def test_usage_failure_has_no_kind(self):
        r = lanbox.parse_guest_line("NETBULK:native:FAIL:usage")
        self.assertEqual((r.kind, r.verdict, r.detail), ("", "FAIL", "usage"))

    def test_output_file_and_terminal_echo(self):
        text = "\n".join(["NETBULK:native:PUT:PASS bytes=100 ms=20 mbps=0.01",
                          "TERM:OUT:NETBULK:native:GET:PASS bytes=100 ms=30 mbps=0.00",
                          "NETBULK:native:done ok=true"])
        results, done = lanbox.parse_output(text)
        self.assertEqual([(r.kind, r.ms, r.pos) for r in results], [("PUT", 20, 0), ("GET", 30, 1)])
        self.assertEqual(len(done), 1)

    def test_typed_command(self):
        self.assertEqual(lanbox.netbulk_command("192.168.1.100", 47810, 16777216, 1, 2),
                         "netbulk 192.168.1.100 47810 16777216 1 2 > /tmp/netbulk.out; cat /tmp/netbulk.out")
        self.assertEqual(lanbox.netbulk_command("10.0.2.2", 1, 2, 3, 4, None), "netbulk 10.0.2.2 1 2 3 4")

    def test_noise_is_ignored(self):
        self.assertIsNone(lanbox.parse_guest_line("WIDGET:TICK:cpu=2 mem=3"))
        self.assertIsNone(lanbox.parse_guest_line("DBGD:AUDIT peer=1.2.3.4 method=ping outcome=ok"))


class CrossCheck(unittest.TestCase):
    SIZE = 1 << 24

    def run_check(self, results, transfers, **kw):
        args = dict(size=self.SIZE, rounds=1, seed=1, peer="192.168.1.81")
        args.update(kw)
        return judge.cross_check(results, transfers, **args)

    def good(self):
        return ([guest("PUT", 6000, self.SIZE, 1), guest("GET", 4000, self.SIZE, 2)],
                [transfer("PUT", 1, self.SIZE, 5.97), transfer("GET", 2, self.SIZE, 3.99)])

    def test_agreement_passes_and_summarises(self):
        rows, problems, warnings = self.run_check(*self.good())
        self.assertEqual((problems, warnings), ([], []))
        self.assertTrue(all(r.ok for r in rows))
        summary = judge.summarise(rows)
        self.assertAlmostEqual(summary["PUT"]["median"], round(self.SIZE / 5.97 / 1e6, 2))
        self.assertIn("GET", summary)
        self.assertIn("192.168.1.81", judge.table(rows))

    def test_timing_disagreement_fails(self):
        results, transfers = self.good()
        results[0] = guest("PUT", 12000, self.SIZE, 1)
        _, problems, _ = self.run_check(results, transfers)
        self.assertTrue(any("timing disagrees" in p for p in problems))

    def test_small_timing_gap_is_tolerated(self):
        results, transfers = self.good()
        results[1] = guest("GET", 4030, self.SIZE, 2)
        self.assertEqual(self.run_check(results, transfers)[1], [])

    def test_traffic_from_another_interface_fails(self):
        results, transfers = self.good()
        transfers = [transfer("PUT", 1, self.SIZE, 5.97, peer="192.168.1.60"),
                     transfer("GET", 2, self.SIZE, 3.99, peer="192.168.1.60")]
        _, problems, _ = self.run_check(results, transfers)
        self.assertTrue(any("came from 192.168.1.60" in p for p in problems))
        self.assertTrue(any("never saw seed 1" in p for p in problems))

    def test_missing_transfer_fails(self):
        results, transfers = self.good()
        rows, problems, _ = self.run_check(results, transfers[:1])
        self.assertTrue(any("round 0 GET" in p and "never saw" in p for p in problems))
        self.assertFalse(rows[1].ok)

    def test_server_problem_fails(self):
        results, transfers = self.good()
        transfers[0] = transfer("PUT", 1, self.SIZE, 5.97, ok=False, received=100)
        _, problems, _ = self.run_check(results, transfers)
        self.assertTrue(any("server:" in p for p in problems))

    def test_guest_failure_fails(self):
        results, transfers = self.good()
        results[1] = guest("GET", 4000, self.SIZE, 2, verdict="FAIL")
        rows, problems, _ = self.run_check(results, transfers)
        self.assertTrue(any("guest: boom" in p for p in problems))
        self.assertFalse(rows[1].ok)

    def test_wrong_byte_count_fails(self):
        results, transfers = self.good()
        results[0] = guest("PUT", 6000, self.SIZE // 2, 1)
        _, problems, _ = self.run_check(results, transfers)
        self.assertTrue(any("guest reports" in p for p in problems))

    def test_no_guest_lines_fails_unless_server_only(self):
        _, transfers = self.good()
        _, problems, _ = self.run_check([], transfers)
        self.assertTrue(any("NETBULK lines were not found" in p for p in problems))
        rows, problems, warnings = self.run_check([], transfers, server_only=True)
        self.assertEqual(problems, [])
        self.assertTrue(warnings)
        self.assertTrue(all(r.ok for r in rows))

    def test_partial_guest_lines_count_the_put_only(self):
        # A desktop Terminal reports the first output line of a command only.
        results, transfers = self.good()
        rows, problems, warnings = self.run_check(results[:1], transfers)
        self.assertEqual(problems, [])
        self.assertTrue(any("1 of 2" in w for w in warnings))
        self.assertTrue(rows[0].ok)
        self.assertFalse(rows[1].ok)
        self.assertNotIn("GET", judge.summarise(rows))

    def test_too_many_guest_lines_fail(self):
        results, transfers = self.good()
        _, problems, _ = self.run_check(results + [guest("PUT", 10, self.SIZE, 3)], transfers)
        self.assertTrue(any("expected 2" in p for p in problems))

    def test_unfinished_transfer_fails(self):
        results, transfers = self.good()
        transfers[1].done = False
        _, problems, _ = self.run_check(results, transfers)
        self.assertTrue(any("did not finish" in p for p in problems))

    def test_two_rounds_use_the_netbulk_seeds(self):
        self.assertEqual(judge.expected_plan(2, 1),
                         [(0, "PUT", 1), (0, "GET", 2), (1, "PUT", 3), (1, "GET", 4)])


class ServerRecords(unittest.TestCase):
    """The real server with a Python client doing what `netbulk` does."""

    def test_put_get_record_peer_and_done(self):
        from bulkpeers import stream
        size = 200_000
        server = BulkServer(0, host="127.0.0.1")
        server.expected(size, 1)
        server.expected(size, 2)
        # port 0: bind first to learn the port.
        probe = socket.socket()
        probe.bind(("127.0.0.1", 0))
        server.port = probe.getsockname()[1]
        probe.close()
        server.start()
        try:
            with socket.create_connection(("127.0.0.1", server.port)) as c:
                c.sendall(f"PUT {size} 1\n".encode() + stream(size, 1))
                c.shutdown(socket.SHUT_WR)
                self.assertTrue(c.recv(64).startswith(b"OK 200000"))
            with socket.create_connection(("127.0.0.1", server.port)) as c:
                c.sendall(f"GET {size} 2\n".encode())
                got = b""
                while chunk := c.recv(65536):
                    got += chunk
                self.assertEqual(got, stream(size, 2))
            lanbox.wait_until(lambda: all(t.done for t in server.transfers) and len(server.transfers) == 2, 5)
        finally:
            server.stop()
        by_kind = {t.kind: t for t in server.transfers}
        self.assertEqual({t.peer for t in server.transfers}, {"127.0.0.1"})
        self.assertTrue(by_kind["PUT"].ok and by_kind["PUT"].done and by_kind["GET"].done)
        rows, problems, _ = judge.cross_check(
            [guest("PUT", max(int(by_kind["PUT"].seconds * 1e3), 1), size, 1),
             guest("GET", max(int(by_kind["GET"].seconds * 1e3), 1), size, 2)],
            server.transfers, size=size, rounds=1, seed=1, peer="127.0.0.1")
        self.assertEqual(problems, [])


class FakeDbgd:
    """Enough of `dbgd` for the reader: handshake, ping, log.tail/follow, msg.topic."""

    def __init__(self, key: str, topics: dict, tail: list[dict], pushed: list[dict], total: int = 5000,
                 files: dict | None = None):
        self.key, self.topics, self.tail, self.pushed, self.total = bytes.fromhex(key), topics, tail, pushed, total
        self.files = files or {}
        self.sock = socket.socket()
        self.sock.bind(("127.0.0.1", 0))
        self.sock.listen(4)
        self.port = self.sock.getsockname()[1]
        threading.Thread(target=self._accept, daemon=True).start()

    def close(self) -> None:
        self.sock.close()

    def _accept(self) -> None:
        while True:
            try:
                conn, _ = self.sock.accept()
            except OSError:
                return
            threading.Thread(target=self._serve, args=(conn,), daemon=True).start()

    def _serve(self, conn: socket.socket) -> None:
        nonce = hashlib.sha256(b"nonce").digest()
        send = lambda obj: conn.sendall(json.dumps(obj).encode() + b"\n")  # noqa: E731
        send({"jsonrpc": "2.0", "method": "hello", "params": {"proto": PROTO, "nonce": nonce.hex()}})
        buffer = b""
        try:
            while True:
                while b"\n" not in buffer:
                    data = conn.recv(65536)
                    if not data:
                        return
                    buffer += data
                line, _, buffer = buffer.partition(b"\n")
                request = json.loads(line)
                method, params, ident = request["method"], request.get("params", {}), request["id"]
                if method == "auth":
                    ok = params["mac"] == client_mac(self.key, nonce)
                    send({"id": ident, "result": {"server_mac": server_mac(self.key, nonce)}} if ok
                         else {"id": ident, "error": {"code": -32001, "message": "bad mac"}})
                elif method == "ping":
                    send({"id": ident, "result": {"uptime_ms": 123456}})
                elif method == "log.tail":
                    send({"id": ident, "result": {"source": "programs", "total": self.total,
                                                  "lines": self.tail[-params.get("lines", 50):]}})
                elif method == "log.follow":
                    send({"id": ident, "result": {"lines": []}})
                    send({"method": "log", "params": {"lines": self.pushed}})
                elif method == "log.unfollow":
                    send({"id": ident, "result": {}})
                elif method == "fs.read" and params["path"] in self.files:
                    send({"id": ident, "result": {"kind": "file", "text": self.files[params["path"]]}})
                elif method == "msg.topic" and params["topic"] in self.topics:
                    send({"id": ident, "result": {"topic": params["topic"], "held": True,
                                                  "payload_hex": self.topics[params["topic"]]}})
                else:
                    send({"id": ident, "error": {"code": -32004, "message": "no such topic"}})
        except OSError:
            return
        finally:
            conn.close()


def line(pos: int, text: str) -> dict:
    return {"pos": pos, "text": text}


class DbgdReader(unittest.TestCase):
    KEY = "00112233445566778899aabbccddeeff"

    def test_read_box_decodes_the_topics(self):
        topics = {"system/net/interfaces": INTERFACES_HEX, "system/net/wlan0/addr": WLAN_ADDR_HEX,
                  "system/net/wlan0/wifi/state": WLAN_STATE_HEX}
        fake = FakeDbgd(self.KEY, topics, [], [])
        try:
            with DbgClient("127.0.0.1", fake.port, self.KEY, timeout=5) as dbg:
                snap = lanbox.read_box(dbg)
        finally:
            fake.close()
        wlan = snap.get("wlan0")
        self.assertEqual((wlan.addr, wlan.prefix, wlan.metric, wlan.primary), ("192.168.1.81", 24, 600, True))
        self.assertEqual((wlan.wifi.ssid, wlan.wifi.band, wlan.wifi.channel), ("ChatonNET", "5 GHz", 100))
        self.assertFalse(snap.get("eth0").has_addr)
        self.assertEqual((snap.uptime_ms, snap.log_total), (123456, 5000))
        self.assertEqual(lanbox.check_route(snap, "wlan0"), ([], []))
        self.assertEqual(snap.primary().name, "wlan0")

    def test_wrong_key_is_refused(self):
        fake = FakeDbgd(self.KEY, {}, [], [])
        try:
            client = DbgClient("127.0.0.1", fake.port, "ff" * 16, timeout=5)
            with self.assertRaises(Exception):
                client.connect()
            client.close()
        finally:
            fake.close()

    def test_guest_log_takes_new_lines_once(self):
        old = line(100, "NETBULK:native:PUT:PASS bytes=1 ms=10 mbps=9.99")
        new_put = line(5100, "NETBULK:native:PUT:PASS bytes=16777216 ms=6230 mbps=2.69")
        new_get = line(5200, "NETBULK:native:GET:PASS bytes=16777216 ms=4000 mbps=4.19")
        done = line(5300, "NETBULK:native:done ok=true")
        noise = line(5150, "DBGD:AUDIT peer=1.2.3.4 method=ping outcome=ok")
        echo = line(5250, "TERM:OUT:NETBULK:native:PUT:PASS bytes=16777216 ms=6230 mbps=2.69")
        tail = [old, new_put, noise]
        fake = FakeDbgd(self.KEY, {}, tail, [])
        log = lanbox.GuestLog(baseline=5000)
        try:
            with DbgClient("127.0.0.1", fake.port, self.KEY, timeout=5) as dbg:
                log.poll(dbg)
                self.assertEqual(len(log.snapshot()[0]), 1)
                # The ring grows; what was taken before is not taken twice.
                tail += [new_get, echo, done]
                log.poll(dbg)
                log.poll(dbg)
        finally:
            fake.close()
        results, finished = log.snapshot()
        self.assertEqual([(r.kind, r.ms) for r in results], [("PUT", 6230), ("GET", 4000), ("PUT", 6230)])
        self.assertEqual(len(finished), 1)

    def test_out_file_is_read_and_missing_file_is_none(self):
        fake = FakeDbgd(self.KEY, {}, [], [], files={"/tmp/netbulk.out": "NETBULK:native:done ok=true\n"})
        try:
            with DbgClient("127.0.0.1", fake.port, self.KEY, timeout=5) as dbg:
                self.assertIn("done ok=true", lanbox.read_out_file(dbg))
                self.assertIsNone(lanbox.read_out_file(dbg, "/tmp/absent"))
        finally:
            fake.close()


if __name__ == "__main__":
    unittest.main(verbosity=1)
