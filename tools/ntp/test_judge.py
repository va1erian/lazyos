#!/usr/bin/env python3
"""The NTP harness's judge fails when it should (docs/ntp-plan.md section 5),
and `fake_ntpd.py` answers the way its behaviours say.

    python tools/ntp/test_judge.py
"""

from __future__ import annotations

import socket
import struct
import sys
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import ntp_judge as judge  # noqa: E402
import scenarios  # noqa: E402
from fake_ntpd import NTP_UNIX, Behaviour, FakeNtpd, Request, honest, ntp_timestamp  # noqa: E402

WINDOW = (1_791_849_600.0, 1_791_849_900.0)
FLOOR = scenarios.FLOOR


def request(index: int, at: float, **kw) -> Request:
    base = dict(index=index, at=at, unix=WINDOW[0] + at, source=("127.0.0.1", 40000), length=48,
                first_byte=0x23, transmit=struct.pack(">Q", 0x1111_0000 + index), other_nonzero=False)
    base.update(kw)
    return Request(**base)


def context(serial: str, requests: list[Request], window=WINDOW) -> judge.Context:
    return judge.Context(serial, requests, window)


UNIX = int(WINDOW[0] + 5)
DEAD_RTC_GOOD = (
    "TIMED:READY unix=1577836800 interface=0x1\n"
    f"TIMED:NTP:FLOOR from=1577836800 to={FLOOR}\n"
    f"TIMED:NTP:STEP offset_ms=1000000 delay_ms=12 stratum=2 server=10.0.2.2 (10.0.2.2) unix={UNIX}\n"
    f"unix_ms={UNIX * 1000 + 321} offset=0 zone=UTC dst=false\n"
)


class Events(unittest.TestCase):
    def test_parsing(self):
        ev = judge.events(DEAD_RTC_GOOD)
        self.assertEqual([e.kind for e in ev], ["FLOOR", "STEP"])
        self.assertEqual(ev[1].number("unix"), UNIX)
        self.assertEqual(ev[1].number("offset_ms"), 1000000)
        self.assertEqual(judge.readings(DEAD_RTC_GOOD), [UNIX * 1000 + 321])

    def test_floor_fail_is_its_own_kind(self):
        ev = judge.events("TIMED:NTP:FLOOR:FAIL from=5 errno=1\n")
        self.assertEqual([e.kind for e in ev], ["FLOOR:FAIL"])

    def test_a_line_after_the_shell_prompt_is_still_an_event(self):
        text = "~ $ TIMED:NTP:REFUSE too large a step after the first sync offset_ms=7208682 server=x\n"
        ev = judge.events(text)
        self.assertEqual([e.kind for e in ev], ["REFUSE"])
        self.assertEqual(ev[0].number("offset_ms"), 7208682)

    def test_the_typed_command_is_not_a_reading(self):
        self.assertEqual(judge.readings("/ # timectl now\n"), [])


class DeadRtc(unittest.TestCase):
    def check(self, serial, requests=None):
        requests = [request(0, 0.0)] if requests is None else requests
        return scenarios.check_dead_rtc(context(serial, requests))

    def test_good_run_passes(self):
        self.assertEqual(self.check(DEAD_RTC_GOOD), [])

    def test_no_floor_fails(self):
        serial = "\n".join(l for l in DEAD_RTC_GOOD.splitlines() if "FLOOR" not in l)
        self.assertTrue(self.check(serial))

    def test_floor_to_the_wrong_value_fails(self):
        self.assertTrue(self.check(DEAD_RTC_GOOD.replace(f"to={FLOOR}", "to=5")))

    def test_step_before_floor_fails(self):
        lines = DEAD_RTC_GOOD.splitlines()
        lines[1], lines[2] = lines[2], lines[1]
        self.assertTrue(any("floor" in p for p in self.check("\n".join(lines))))

    def test_step_to_the_wrong_time_fails(self):
        self.assertTrue(self.check(DEAD_RTC_GOOD.replace(f"unix={UNIX}", f"unix={FLOOR}")))

    def test_clock_reading_outside_the_window_fails(self):
        self.assertTrue(self.check(DEAD_RTC_GOOD.replace(f"unix_ms={UNIX * 1000 + 321}", f"unix_ms={FLOOR * 1000}")))

    def test_missing_reading_fails(self):
        serial = "\n".join(l for l in DEAD_RTC_GOOD.splitlines() if not l.startswith("unix_ms"))
        self.assertTrue(any("never printed" in p for p in self.check(serial)))

    def test_bad_request_shape_fails(self):
        for bad in (dict(length=47), dict(first_byte=0x1B), dict(transmit=bytes(8)), dict(other_nonzero=True)):
            self.assertTrue(self.check(DEAD_RTC_GOOD, [request(0, 0.0, **bad)]), bad)

    def test_repeated_nonce_fails(self):
        a, b = request(0, 0.0), request(1, 20.0, transmit=request(0, 0.0).transmit)
        self.assertTrue(self.check(DEAD_RTC_GOOD, [a, b]))


class Spacing(unittest.TestCase):
    def test_minimum_interval(self):
        ok = [request(0, 0.0), request(1, 15.2), request(2, 30.5)]
        self.assertEqual(judge.minimum_interval(ok), [])
        bad = [request(0, 0.0), request(1, 5.0)]
        self.assertTrue(judge.minimum_interval(bad))

    def test_silent_needs_growth_and_spacing(self):
        fails = "".join("TIMED:NTP:FAIL no usable answer asked=1 answered=0\n" for _ in range(3))
        ok = [request(i, t) for i, t in enumerate([0.0, 17.0, 34.0, 55.0])]
        self.assertEqual(scenarios.check_silent(context(fails, ok)), [])
        hammering = [request(i, i * 3.0) for i in range(4)]
        self.assertTrue(scenarios.check_silent(context(fails, hammering)))
        self.assertTrue(scenarios.check_silent(context("", ok[:1])))
        stepped = fails + f"TIMED:NTP:STEP offset_ms=5 delay_ms=1 stratum=2 server=x unix={UNIX}\n"
        self.assertTrue(scenarios.check_silent(context(stepped, ok)))


class Resync(unittest.TestCase):
    def serial(self, second: str) -> str:
        return ("TIMED:NTP:STEP offset_ms=900 delay_ms=5 stratum=2 server=x" + chr(10)
                + second + chr(10) + f"unix_ms={UNIX * 1000} offset=0" + chr(10))

    def test_a_second_step_or_sync_is_taken(self):
        reqs = [request(0, 0.0), request(1, 22.0)]
        for second in ("TIMED:NTP:SYNC offset_ms=-8 delay_ms=5 stratum=2 server=x",
                       "TIMED:NTP:STEP offset_ms=5049 delay_ms=7 stratum=2 server=x unix=1"):
            self.assertEqual(scenarios.check_resync(context(self.serial(second), reqs)), [], second)

    def test_a_refusal_a_wild_offset_or_no_second_exchange_fails(self):
        reqs = [request(0, 0.0), request(1, 22.0)]
        refused = self.serial("TIMED:NTP:REFUSE too large a step after the first sync offset_ms=7208682 server=x")
        self.assertTrue(scenarios.check_resync(context(refused, reqs)))
        wild = self.serial("TIMED:NTP:STEP offset_ms=900000 delay_ms=5 stratum=2 server=x unix=1")
        self.assertTrue(scenarios.check_resync(context(wild, reqs)))
        self.assertTrue(scenarios.check_resync(context(self.serial(""), [request(0, 0.0)])))


class Deny(unittest.TestCase):
    def test_a_denied_address_is_asked_once(self):
        good = ("TIMED:NTP:DENY 10.0.2.2 will not be asked again\n"
                "TIMED:NTP:FAIL no usable answer asked=1 answered=1\n"
                "TIMED:NTP:FAIL every server address asked not to be queried\n")
        self.assertEqual(scenarios.check_deny(context(good, [request(0, 0.0)])), [])
        again = [request(0, 0.0), request(1, 20.0)]
        self.assertTrue(scenarios.check_deny(context(good, again)))
        self.assertTrue(scenarios.check_deny(context("", [request(0, 0.0)])))


class Restart(unittest.TestCase):
    def test_a_restarted_timed_is_seen(self):
        self.assertEqual(judge.timed_started_once("TIMED:READY unix=1 interface=0x1\n"), [])
        self.assertTrue(judge.timed_started_once(""))
        self.assertTrue(judge.timed_started_once("TIMED:READY a\nTIMED:READY b\n"))

    def test_constant_retry_gaps_are_not_a_back_off(self):
        fails = "".join("TIMED:NTP:FAIL no usable answer asked=1 answered=0\n" for _ in range(3))
        constant = [request(i, 15.5 * i) for i in range(4)]
        self.assertTrue(scenarios.check_silent(context(fails, constant)))


class Kiss(unittest.TestCase):
    def test_rate_means_one_request(self):
        serial = "TIMED:NTP:RATE asked=1\n"
        self.assertEqual(scenarios.check_rate(context(serial, [request(0, 0.0)])), [])
        self.assertTrue(scenarios.check_rate(context(serial, [request(0, 0.0), request(1, 20.0)])))
        self.assertTrue(scenarios.check_rate(context("", [request(0, 0.0)])))


class Hostile(unittest.TestCase):
    def serial(self, extra=""):
        lines = ["TIMED:NTP:FAIL no usable answer asked=1 answered=0"] * 6
        lines.append("TIMED:NTP:REFUSE before the floor offset_ms=-1790000000 server=x (y)")
        lines.append(f"unix_ms={UNIX * 1000} offset=0 zone=UTC dst=false")
        return "\n".join(lines) + "\n" + extra

    def test_good(self):
        self.assertEqual(scenarios.check_hostile(context(self.serial(), [request(i, i * 20.0) for i in range(7)])), [])

    def test_a_believed_answer_fails(self):
        extra = f"TIMED:NTP:SYNC offset_ms=1 delay_ms=1 stratum=2 server=x\n"
        self.assertTrue(scenarios.check_hostile(context(self.serial(extra), [request(0, 0.0)])))

    def test_missing_verdicts_fail(self):
        short = "TIMED:NTP:FAIL x\n" + f"unix_ms={UNIX * 1000} offset=0\n"
        self.assertTrue(scenarios.check_hostile(context(short, [request(0, 0.0)])))


class RefuseLarge(unittest.TestCase):
    def test_needs_a_too_large_refusal_and_a_sane_clock(self):
        good = ("TIMED:NTP:SYNC offset_ms=5 delay_ms=1 stratum=2 server=x\n"
                "TIMED:NTP:REFUSE too large a step after the first sync offset_ms=7200000 server=x\n"
                f"unix_ms={UNIX * 1000} offset=0\n")
        reqs = [request(0, 0.0), request(1, 25.0)]
        self.assertEqual(scenarios.check_refuse_large(context(good, reqs)), [])
        self.assertTrue(scenarios.check_refuse_large(context(good.replace("too large", "other"), reqs)))
        stepped = good + f"TIMED:NTP:STEP offset_ms=7200000 delay_ms=1 stratum=2 server=x unix={UNIX + 7200}\n"
        stepped += f"TIMED:NTP:STEP offset_ms=7200000 delay_ms=1 stratum=2 server=x unix={UNIX + 7200}\n"
        self.assertTrue(scenarios.check_refuse_large(context(stepped, reqs)))


class Catalogue(unittest.TestCase):
    def test_names_are_unique_and_scripted(self):
        names = [s.name for s in scenarios.catalogue()]
        self.assertEqual(len(names), len(set(names)))
        for s in scenarios.catalogue():
            self.assertTrue(s.script and s.steps and s.what, s.name)


class FakeServer(unittest.TestCase):
    """The server answers as its behaviours say, over a real socket."""

    def ask(self, server: FakeNtpd, nonce: bytes = b"\xaa" * 8, wait: float = 0.5):
        sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        sock.settimeout(wait)
        packet = bytes([0x23]) + bytes(39) + nonce
        sock.sendto(packet, ("127.0.0.1", server.port))
        try:
            return sock.recvfrom(512)[0]
        except socket.timeout:
            return None
        finally:
            sock.close()

    def test_behaviours(self):
        script = [honest(100.0), Behaviour("rate"), Behaviour("silent"), Behaviour("alarm"),
                  Behaviour("short"), Behaviour("ancient")]
        server = FakeNtpd(47199, script)
        try:
            reply = self.ask(server)
            self.assertEqual(len(reply), 48)
            self.assertEqual(reply[0] & 7, 4)
            self.assertEqual(reply[24:32], b"\xaa" * 8)
            secs = struct.unpack(">I", reply[40:44])[0] - NTP_UNIX
            self.assertAlmostEqual(secs, server.snapshot()[0].unix + 100, delta=2)
            rate = self.ask(server)
            self.assertEqual((rate[1], rate[12:16]), (0, b"RATE"))
            self.assertIsNone(self.ask(server))
            self.assertEqual(self.ask(server)[0] >> 6, 3)
            self.assertEqual(len(self.ask(server)), 20)
            ancient = self.ask(server)
            self.assertLess(struct.unpack(">I", ancient[40:44])[0] - NTP_UNIX, 10)
            log = server.snapshot()
            self.assertEqual([r.behaviour for r in log], ["honest", "rate", "silent", "alarm", "short", "ancient"])
            self.assertTrue(all(r.length == 48 and r.version == 4 and r.mode == 3 for r in log))
        finally:
            server.close()

    def test_timestamp_encoding(self):
        self.assertEqual(ntp_timestamp(0.5), struct.pack(">II", NTP_UNIX, 1 << 31))


if __name__ == "__main__":
    unittest.main()
