"""One station as an access point sees it: authentication and association
state, the 4-way handshake and group key handshake as the authenticator
(IEEE 802.11-2020 12.7.6, 12.7.7), and the station's keys and packet
numbers. The cryptography is `wpa.py`'s; nothing here is shared with the
guest's `libs/eapol`.
"""

from __future__ import annotations

import os
from dataclasses import dataclass, field

from . import wpa
from .dot11 import RSN_WPA2_PSK

#: Retransmissions of message 1 and message 3 before the AP gives up
#: (reason 15, "4-way handshake timeout"), and their interval.
RETRIES = 4
RETRY_S = 1.0


@dataclass
class Bss:
    ssid: str
    channel: int
    bssid: bytes
    security: str = "open"          # "open" or "wpa2"
    passphrase: str | None = None
    hidden: bool = False
    rssi: int = -45
    ht: bool = True
    band: int = 0
    # The group key: (key id, key, last PN used).
    gtk_index: int = 1
    gtk: bytes = field(default_factory=lambda: os.urandom(16))
    gtk_pn: int = 0
    seq: int = 0
    pmk: bytes = b""

    def __post_init__(self) -> None:
        if self.security == "wpa2":
            self.pmk = wpa.pmk(self.passphrase or "", self.ssid.encode())

    def next_seq(self) -> int:
        self.seq = (self.seq + 1) & 0xFFF
        return self.seq

    # A new group key on its way to the stations; the AP keeps sending
    # with the old one until a station confirmed it (group message 2).
    pending_index: int = 0
    pending_gtk: bytes = b""

    def rekey(self) -> None:
        self.pending_index = 2 if self.gtk_index == 1 else 1
        self.pending_gtk = os.urandom(16)

    def commit_rekey(self) -> None:
        if self.pending_gtk:
            self.gtk_index, self.gtk, self.gtk_pn = self.pending_index, self.pending_gtk, 0
            self.pending_gtk = b""


@dataclass
class Station:
    mac: bytes
    bss: Bss
    state: str = "authenticated"    # then "associated", "handshake", "authorized"
    aid: int = 0
    assoc_rsn: bytes = b""
    anonce: bytes = b""
    replay: int = 0
    ptk: wpa.Ptk | None = None
    pending: wpa.Ptk | None = None
    tk_installed: bool = False
    tx_pn: int = 0
    rx_pn: dict[int, int] = field(default_factory=dict)
    # The EAPOL message the AP waits an answer to, when it was sent, and how
    # many times.
    waiting: str | None = None
    sent_at: float = 0.0
    tries: int = 0
    rekey_index: int = 0
    rekey_key: bytes = b""
    bad_mics: int = 0
    pings: int = 0
    next_ping: float = 0.0

    @property
    def authorized(self) -> bool:
        return self.state == "authorized"

    def rx_fresh(self, tid: int | None, pn: int) -> bool:
        slot = 16 if tid is None else tid
        if pn <= self.rx_pn.get(slot, 0):
            return False
        self.rx_pn[slot] = pn
        return True

    # Messages the AP sends. Each returns the EAPOL frame.

    def message1(self, now: float) -> bytes:
        if self.waiting != "m1":
            self.anonce = wpa.nonce()
            self.tries = 0
        self.replay += 1
        self.waiting, self.sent_at = "m1", now
        self.tries += 1
        return wpa.build(wpa.MSG1, 16, self.replay, nonce=self.anonce)

    def message3(self, now: float) -> bytes:
        if self.waiting != "m3":
            self.tries = 0
        self.replay += 1
        self.waiting, self.sent_at = "m3", now
        self.tries += 1
        ptk = self.pending
        bss = self.bss
        plain = RSN_WPA2_PSK + wpa.gtk_kde(bss.gtk_index, bss.gtk)
        rsc = bss.gtk_pn.to_bytes(8, "little")
        return wpa.build(wpa.MSG3, 16, self.replay, nonce=self.anonce, rsc=rsc,
                         data=wpa.wrap(ptk.kek, plain), kck=ptk.kck)

    def group1(self, now: float) -> bytes:
        if self.waiting != "g1":
            self.tries = 0
        self.replay += 1
        self.waiting, self.sent_at = "g1", now
        self.tries += 1
        bss = self.bss
        self.rekey_index, self.rekey_key = bss.pending_index, bss.pending_gtk
        # A new key: its packet numbers start at 0.
        rsc = bytes(8)
        data = wpa.wrap(self.ptk.kek, wpa.gtk_kde(self.rekey_index, self.rekey_key))
        return wpa.build(wpa.GROUP1, 0, self.replay, rsc=rsc, data=data, kck=self.ptk.kck)

    def resend(self, now: float) -> bytes | None:
        """The message to send again, if one is due."""
        if self.waiting is None or now - self.sent_at < RETRY_S:
            return None
        return {"m1": self.message1, "m3": self.message3, "g1": self.group1}[self.waiting](now)

    def timed_out(self) -> bool:
        return self.waiting is not None and self.tries > RETRIES

    def answer(self, frame: bytes, now: float) -> tuple[list[str], bytes | None]:
        """A station's EAPOL frame: the log lines and the AP's reply."""
        key = wpa.parse(frame)
        if key is None:
            return ["AP:EAPOL:MALFORMED"], None
        if not key.info & wpa.MIC:
            return ["AP:EAPOL:NO_MIC"], None
        if key.replay != self.replay:
            return [f"AP:EAPOL:STALE replay={key.replay} want={self.replay}"], None
        pairwise = bool(key.info & wpa.PAIRWISE)
        if self.waiting == "m1" and pairwise and not key.info & wpa.SECURE:
            ptk = wpa.ptk(self.bss.pmk, self.bss.bssid, self.mac, self.anonce, key.nonce)
            if not wpa.verify(ptk.kck, key):
                self.bad_mics += 1
                return ["AP:EAPOL:M2:BADMIC"], None
            if key.data != self.assoc_rsn:
                return ["AP:EAPOL:M2:RSN_MISMATCH"], None
            self.pending = ptk
            return ["AP:EAPOL:M2:OK", "AP:EAPOL:M3"], self.message3(now)
        if self.waiting == "m3" and pairwise and key.info & wpa.SECURE:
            if not wpa.verify(self.pending.kck, key):
                return ["AP:EAPOL:M4:BADMIC"], None
            self.ptk, self.pending, self.waiting = self.pending, None, None
            self.tk_installed, self.tx_pn, self.rx_pn = True, 0, {}
            self.state = "authorized"
            return ["AP:EAPOL:M4:OK", "AP:PTK:INSTALLED"], None
        if self.waiting == "g1" and not pairwise:
            if not wpa.verify(self.ptk.kck, key):
                return ["AP:REKEY:G2:BADMIC"], None
            self.waiting = None
            self.bss.commit_rekey()
            return [f"AP:REKEY:G2:OK index={self.rekey_index}"], None
        return [f"AP:EAPOL:UNEXPECTED info={key.info:#06x}"], None
