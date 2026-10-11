"""The fake access points: beacons, probe responses, open authentication,
association, the authenticator (`station.py`), CCMP data both ways, and the
network behind them (`netsvc.py`), on one simulated air.

`World.receive` takes every frame the guest's radio sends; `World.tick`
sends beacons, retransmits handshake messages and pings. Everything the AP
does or refuses is a log line (`AP:...`) the judge reads; every frame on the
air goes to the pcap.
"""

from __future__ import annotations

import struct
import time
from typing import Callable

from . import ccmp, dot11, netsvc
from .air import Air, PcapWriter
from .dot11 import mac_text
from .station import Bss, Station

BEACON_TU = 100
BEACON_S = BEACON_TU * 1024 / 1e6
#: Pings the gateway sends a station after its DHCP lease, one a second.
PINGS = 5
#: Deauthentication reasons (9.4.1.7).
REASON_UNSPECIFIED, REASON_HANDSHAKE_TIMEOUT = 1, 15


class World:
    def __init__(self, bsses: list[Bss], send: Callable[[Air, bytes], None], pcap: PcapWriter,
                 log: Callable[[str], None]):
        self.bsses = bsses
        self._send = send
        self.pcap = pcap
        self.log = log
        self.stations: dict[tuple[bytes, bytes], Station] = {}
        self.network = netsvc.Network()
        self.started = time.monotonic()
        self.next_beacon = 0.0
        self.silent_until = 0.0
        self.first_authorized: float | None = None

    # The air.

    def send(self, bss: Bss, frame: bytes) -> None:
        self.pcap.write(frame)
        self._send(Air(bss.band, bss.channel, bss.rssi, 0), frame)

    def silent(self, now: float) -> bool:
        return now < self.silent_until

    def tick(self, now: float) -> float:
        """Timed work; returns when it is next due."""
        if now >= self.next_beacon:
            self.next_beacon = now + BEACON_S
            if not self.silent(now):
                for bss in self.bsses:
                    self.beacon(bss, now)
        for station in list(self.stations.values()):
            self.station_timers(station, now)
        return self.next_beacon

    def beacon(self, bss: Bss, now: float) -> None:
        ssid = b"" if bss.hidden else bss.ssid.encode()
        stamp = int(max(0.0, now - self.started) * 1e6)
        body = dot11.beacon_body(ssid, bss.channel, BEACON_TU, bss.security == "wpa2", bss.ht, stamp)
        self.send(bss, dot11.mgmt(dot11.BEACON, dot11.BROADCAST, bss.bssid, bss.bssid, bss.next_seq(), body))

    def station_timers(self, station: Station, now: float) -> None:
        if self.silent(now):
            return
        if station.timed_out():
            self.log(f"AP:EAPOL:TIMEOUT sta={mac_text(station.mac)} waiting={station.waiting} "
                     f"ssid={station.bss.ssid}")
            self.deauth(station, REASON_HANDSHAKE_TIMEOUT)
            return
        again = station.resend(now)
        if again is not None:
            self.log(f"AP:EAPOL:RESEND {station.waiting} try={station.tries}")
            self.send_eapol(station, again)
        if station.authorized and station.pings < PINGS and now >= station.next_ping:
            packet = self.network.ping(station.mac, station.pings + 1)
            if packet is not None:
                station.pings += 1
                station.next_ping = now + 1.0
                self.log(f"AP:ICMP:PING:SENT to={mac_text(station.mac)} seq={station.pings} "
                         f"ssid={station.bss.ssid}")
                self.send_ethernet(station, packet)

    # Frames from the guest.

    def receive(self, air: Air, frame: bytes, now: float) -> None:
        self.pcap.write(frame)
        if self.silent(now):
            return
        header = dot11.parse_header(frame)
        if header is None:
            return
        for bss in self.bsses:
            if bss.channel != air.channel or bss.band != air.band:
                continue
            if header.ftype == dot11.TYPE_MGMT:
                self.on_mgmt(bss, header, frame, now)
            elif header.ftype == dot11.TYPE_DATA and header.addr1 == bss.bssid:
                self.on_data(bss, header, frame, now)

    def on_mgmt(self, bss: Bss, h: dot11.Header, frame: bytes, now: float) -> None:
        body = frame[24:]
        if h.subtype == dot11.PROBE_REQ:
            ssid = dot11.elements(body).get(dot11.IE_SSID, b"")
            if (not ssid and not bss.hidden) or ssid == bss.ssid.encode():
                self.log(f"AP:PROBE ssid={bss.ssid} ch={bss.channel} wildcard={int(not ssid)}")
                info = dot11.beacon_body(bss.ssid.encode(), bss.channel, BEACON_TU, bss.security == "wpa2",
                                         bss.ht, int(max(0.0, now - self.started) * 1e6))
                self.send(bss, dot11.mgmt(dot11.PROBE_RESP, h.addr2, bss.bssid, bss.bssid, bss.next_seq(), info))
            return
        if h.addr1 != bss.bssid or h.addr3 != bss.bssid:
            return
        key = (bss.bssid, h.addr2)
        if h.subtype == dot11.AUTH and len(body) >= 6:
            algorithm, transaction, _ = struct.unpack("<HHH", body[:6])
            status = 0 if algorithm == 0 and transaction == 1 else 13
            self.log(f"AP:AUTH ssid={bss.ssid} sta={mac_text(h.addr2)} status={status}")
            if status == 0:
                self.stations[key] = Station(h.addr2, bss)
            reply = struct.pack("<HHH", algorithm, 2, status)
            self.send(bss, dot11.mgmt(dot11.AUTH, h.addr2, bss.bssid, bss.bssid, bss.next_seq(), reply))
        elif h.subtype in (dot11.ASSOC_REQ, dot11.REASSOC_REQ) and key in self.stations:
            self.on_assoc(bss, self.stations[key], h, body, now)
        elif h.subtype in (dot11.DEAUTH, dot11.DISASSOC) and key in self.stations:
            reason = struct.unpack("<H", body[:2])[0] if len(body) >= 2 else 0
            self.log(f"AP:STA:LEFT sta={mac_text(h.addr2)} reason={reason}")
            del self.stations[key]

    def on_assoc(self, bss: Bss, station: Station, h: dot11.Header, body: bytes, now: float) -> None:
        fixed = 10 if h.subtype == dot11.REASSOC_REQ else 4
        ies = dot11.elements(body[fixed:])
        rsn = ies.get(dot11.IE_RSN)
        status = 0
        if ies.get(dot11.IE_SSID) != bss.ssid.encode():
            status = 1
        elif bss.security == "wpa2" and (rsn is None or bytes([dot11.IE_RSN, len(rsn)]) + rsn != dot11.RSN_WPA2_PSK):
            status = 43  # invalid AKMP (or cipher): not WPA2-PSK with CCMP
        elif bss.security == "open" and rsn is not None:
            status = 1
        station.aid = 1 + sum(1 for s in self.stations.values() if s.bss is bss and s.aid)
        self.log(f"AP:ASSOC ssid={bss.ssid} sta={mac_text(station.mac)} status={status} aid={station.aid}")
        caps = dot11.CAP_ESS | (dot11.CAP_PRIVACY if bss.security == "wpa2" else 0)
        reply = struct.pack("<HHH", caps, status, station.aid | 0xC000) + dot11.ie(dot11.IE_RATES, dot11.RATES)
        if bss.ht:
            reply += dot11.ie(dot11.IE_HT_CAP, bytes(26))
        self.send(bss, dot11.mgmt(dot11.ASSOC_RESP, station.mac, bss.bssid, bss.bssid, bss.next_seq(), reply))
        if status:
            return
        if bss.security == "open":
            station.state = "authorized"
            self.authorized(station, now)
        else:
            station.state = "handshake"
            station.assoc_rsn = bytes([dot11.IE_RSN, len(rsn)]) + rsn
            self.log(f"AP:EAPOL:M1 ssid={bss.ssid}")
            self.send_eapol(station, station.message1(now))

    def authorized(self, station: Station, now: float) -> None:
        self.log(f"AP:AUTHORIZED ssid={station.bss.ssid} sta={mac_text(station.mac)} "
                 f"security={station.bss.security}")
        station.next_ping = now + 1.0
        if self.first_authorized is None:
            self.first_authorized = now

    def on_data(self, bss: Bss, h: dot11.Header, frame: bytes, now: float) -> None:
        station = self.stations.get((bss.bssid, h.addr2))
        if station is None or not h.flags & dot11.TO_DS or h.subtype & 0x04:
            return
        if h.protected:
            if station.ptk is None:
                self.log("AP:CCMP:NOKEY")
                return
            parsed = ccmp.parse_ccmp_header(frame[h.length:])
            body = ccmp.open_frame(station.ptk.tk, frame, h.length)
            if body is None or parsed is None:
                self.log(f"AP:CCMP:BADMIC sta={mac_text(station.mac)} ssid={bss.ssid}")
                return
            if not station.rx_fresh(h.tid, parsed[0]):
                self.log(f"AP:CCMP:REPLAY pn={parsed[0]} ssid={bss.ssid}")
                return
            self.log_once(station, f"AP:CCMP:RX:OK ssid={bss.ssid}")
        else:
            body = frame[h.length:]
        msdu = dot11.unsnap(body)
        if msdu is None:
            self.log("AP:DATA:BADLLC")
            return
        ethertype, payload = msdu
        if ethertype == dot11.ETHERTYPE_EAPOL:
            lines, reply = station.answer(payload, now)
            for line in lines:
                self.log(f"{line} ssid={bss.ssid}")
            if reply is not None:
                self.send_eapol(station, reply)
            if station.authorized and station.bss.security == "wpa2" and "AP:PTK:INSTALLED" in lines:
                self.authorized(station, now)
            return
        if station.ptk is not None and not h.protected:
            self.log("AP:DATA:PLAINTEXT:DROPPED")
            return
        if not station.authorized:
            self.log("AP:DATA:NOT_AUTHORIZED")
            return
        eth = h.addr3 + h.addr2 + struct.pack(">H", ethertype) + payload
        for answer in self.network.handle(eth):
            self.send_ethernet(station, answer)
        for line in self.network.events:
            self.log(f"{line} ssid={bss.ssid}")
        self.network.events.clear()

    def log_once(self, station: Station, line: str) -> None:
        if not getattr(station, "_logged_" + line, False):
            setattr(station, "_logged_" + line, True)
            self.log(line)

    # Frames to the guest.

    def send_eapol(self, station: Station, frame: bytes) -> None:
        eth = station.mac + station.bss.bssid + struct.pack(">H", dot11.ETHERTYPE_EAPOL) + frame
        self.send_ethernet(station, eth)

    def send_ethernet(self, station: Station, eth: bytes) -> None:
        """An Ethernet frame to `station` (or, group-addressed, to the BSS) as
        a From-DS data frame: under the pairwise key once installed, under
        the group key for a group address on a WPA2 network."""
        bss = station.bss
        da, sa, ethertype = eth[:6], eth[6:12], struct.unpack(">H", eth[12:14])[0]
        msdu = dot11.snap(ethertype, eth[14:])
        group = bool(da[0] & 1)
        security = None
        if group and bss.security == "wpa2":
            bss.gtk_pn += 1
            security, key = ccmp.ccmp_header(bss.gtk_pn, bss.gtk_index), bss.gtk
        elif not group and station.tk_installed:
            station.tx_pn += 1
            security, key = ccmp.ccmp_header(station.tx_pn, 0), station.ptk.tk
        frame = dot11.data_frame(da, bss.bssid, sa, bss.next_seq(), msdu, qos=bss.ht, security=security)
        if security is not None:
            frame = ccmp.seal(key, frame, 26 if bss.ht else 24)
        self.send(bss, frame)

    # Faults.

    def deauth(self, station: Station, reason: int) -> None:
        bss = station.bss
        body = struct.pack("<H", reason)
        self.send(bss, dot11.mgmt(dot11.DEAUTH, station.mac, bss.bssid, bss.bssid, bss.next_seq(), body))
        self.log(f"AP:DEAUTH sta={mac_text(station.mac)} reason={reason}")
        self.stations.pop((bss.bssid, station.mac), None)

    def fault(self, command: str, now: float) -> None:
        words = command.split()
        if not words:
            return
        if words[0] == "deauth":
            reason = int(words[1]) if len(words) > 1 else REASON_UNSPECIFIED
            self.log(f"AP:FAULT:DEAUTH reason={reason}")
            for station in [s for s in self.stations.values() if s.authorized]:
                self.deauth(station, reason)
        elif words[0] == "silence":
            seconds = float(words[1]) if len(words) > 1 else 5.0
            self.silent_until = now + seconds
            self.log(f"AP:FAULT:SILENCE seconds={seconds}")
        elif words[0] == "rekey":
            for bss in {id(s.bss): s.bss for s in self.stations.values() if s.authorized}.values():
                if bss.security != "wpa2":
                    continue
                bss.rekey()
                self.log(f"AP:FAULT:REKEY ssid={bss.ssid} index={bss.pending_index}")
                for station in self.stations.values():
                    if station.bss is bss and station.authorized:
                        self.log("AP:REKEY:G1")
                        self.send_eapol(station, station.group1(now))
        else:
            self.log(f"AP:FAULT:UNKNOWN {command}")
