"""Reading a LazyOS box over its `dbgd` for the LAN throughput tool
(`tools/net/lan_bulk.py`): which interface carries its traffic, the Wi-Fi
link, and the `NETBULK:` lines its programs print.

Everything here is read-only. The retained Messenger topics come back from
`dbgd` as the raw parcel (`msg.topic`: a 48-byte header, then the TLV body of
`docs/midl.md` "Wire encoding"), so this module decodes the few structs it
needs from `idl/net.midl` and `idl/wifi.midl` by schema:

* `system/net/interfaces` (`InterfaceList`): link, metric, which one is the
  primary (the best default route);
* `system/net/<if>/addr` (`AddressEvent`): address, prefix, gateway;
* `system/net/<if>/wifi/state` (`WifiState`): SSID, signal, frequency.

The `NETBULK:` markers are the guest client's output
(`user/src/bin/netbulk.rs`), read from `dbgd`'s "programs" log ring.
"""

from __future__ import annotations

import re
import struct
import sys
import time
from dataclasses import dataclass, field
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "dbg"))
from dbgclient import DbgClient, DbgError  # noqa: E402

HEADER = 48
#: The parcel header's `body_len` field.
BODY_LEN_AT = 20
KIND_STRING, KIND_BYTES, KIND_STRUCT = 7, 8, 10

NIC_KINDS = ["wired", "wireless"]
DHCP_STATES = ["off", "discovering", "bound"]
CONN_STATES = ["disabled", "idle", "scanning", "authenticating", "associating",
               "handshake", "connected", "failed"]

INTERFACE = {1: ("name", "str"), 2: ("mac", "mac"), 3: ("mtu", "u32"), 4: ("link", "bool"),
             5: ("mode", "u32"), 6: ("dhcp", "u32"), 7: ("kind", "u32"), 8: ("metric", "u32"),
             9: ("primary", "bool")}
INTERFACE_LIST = {1: ("list", ("array", INTERFACE))}
ADDRESS = {1: ("interface", "str"), 2: ("addr", "ip"), 3: ("prefix", "u32"), 4: ("gateway", "ip")}
WIFI_STATE = {1: ("state", "u32"), 2: ("ssid", "str"), 3: ("bssid", "mac"), 4: ("rssi_dbm", "i32"),
              5: ("freq_mhz", "u32"), 6: ("id", "str"), 7: ("reason", "u32"), 8: ("changes", "u32")}


class TlvError(ValueError):
    """A topic payload that does not parse."""


# -- TLV ---------------------------------------------------------------------

def parse_tlv(buf: bytes) -> list[tuple[int, int, bytes]]:
    """`(kind, id, value)` of every field in a body."""
    out, at = [], 0
    while at < len(buf):
        if at + 8 > len(buf):
            raise TlvError("a field header runs past the body")
        tag, length = struct.unpack_from("<II", buf, at)
        at += 8
        if at + length > len(buf):
            raise TlvError("a field runs past the body")
        out.append((tag & 0xFF, tag >> 8, buf[at:at + length]))
        at += length
    return out


def _convert(kind: str | tuple, raw: bytes):
    if isinstance(kind, tuple):
        sub = kind[1]
        return [decode_struct(value, sub) for k, _, value in parse_tlv(raw) if k == KIND_STRUCT]
    if kind == "bool":
        return raw[:1] not in (b"", b"\0")
    if kind in ("i32", "u32"):
        if len(raw) != 4:
            raise TlvError(f"a {kind} of {len(raw)} bytes")
        return struct.unpack("<i" if kind == "i32" else "<I", raw)[0]
    if kind == "str":
        return raw.decode("utf-8", "replace")
    if kind == "ip":
        return ".".join(map(str, raw)) if len(raw) == 4 else "0.0.0.0"
    if kind == "mac":
        return ":".join(f"{b:02x}" for b in raw)
    return raw


def _default(kind: str | tuple):
    if isinstance(kind, tuple):
        return []
    return {"bool": False, "i32": 0, "u32": 0, "str": "", "ip": "0.0.0.0", "mac": ""}.get(kind, b"")


def decode_struct(body: bytes, schema: dict) -> dict:
    """A struct's fields by name; absent fields keep their default."""
    out = {name: _default(kind) for name, kind in schema.values()}
    for _, ident, raw in parse_tlv(body):
        if ident in schema:
            name, kind = schema[ident]
            out[name] = _convert(kind, raw)
    return out


def topic_value(result: dict, schema: dict) -> dict:
    """Decode a `msg.topic` result (`payload_hex`) as `schema`."""
    payload = bytes.fromhex(result.get("payload_hex", ""))
    if len(payload) < HEADER:
        raise TlvError("a payload shorter than the parcel header")
    (body_len,) = struct.unpack_from("<I", payload, BODY_LEN_AT)
    body = payload[HEADER:HEADER + body_len]
    fields = parse_tlv(body)
    # The topic's value is one field holding the encoded struct: a Bytes
    # field, or a String one when its bytes happen to be valid UTF-8 (a
    # 10.0.2.15 address), so either unwraps.
    if len(fields) == 1 and fields[0][0] in (KIND_STRING, KIND_BYTES):
        body = fields[0][2]
    return decode_struct(body, schema)


# -- the box's network state -------------------------------------------------

def channel_of(freq_mhz: int) -> tuple[str, int]:
    """`(band, channel)` of a centre frequency; `("", 0)` when unknown."""
    if freq_mhz == 2484:
        return "2.4 GHz", 14
    if 2412 <= freq_mhz <= 2472:
        return "2.4 GHz", (freq_mhz - 2407) // 5
    if 5160 <= freq_mhz <= 5895:
        return "5 GHz", (freq_mhz - 5000) // 5
    if 5955 <= freq_mhz <= 7115:
        return "6 GHz", (freq_mhz - 5950) // 5
    return "", 0


@dataclass
class WifiLink:
    state: str = ""
    ssid: str = ""
    bssid: str = ""
    rssi_dbm: int = 0
    freq_mhz: int = 0
    band: str = ""
    channel: int = 0

    @classmethod
    def from_topic(cls, value: dict) -> "WifiLink":
        band, channel = channel_of(value["freq_mhz"])
        state = value["state"]
        return cls(CONN_STATES[state] if state < len(CONN_STATES) else str(state), value["ssid"],
                   value["bssid"], value["rssi_dbm"], value["freq_mhz"], band, channel)

    def describe(self) -> str:
        if self.state != "connected":
            return f"wifi {self.state or 'unknown'}"
        where = f"{self.freq_mhz} MHz" + (f" ({self.band} ch {self.channel})" if self.band else "")
        return f"{self.ssid or self.bssid}, {where}, rssi {self.rssi_dbm} dBm"


@dataclass
class Iface:
    name: str
    mac: str = ""
    link: bool = False
    kind: str = "wired"
    dhcp: str = ""
    metric: int = 0
    primary: bool = False
    addr: str = "0.0.0.0"
    prefix: int = 0
    gateway: str = "0.0.0.0"
    wifi: WifiLink | None = None

    @property
    def has_addr(self) -> bool:
        return self.addr != "0.0.0.0"


@dataclass
class BoxSnapshot:
    uptime_ms: int = 0
    ifaces: list[Iface] = field(default_factory=list)
    #: The programs log's size when read, so older lines can be told apart.
    log_total: int = 0

    def get(self, name: str) -> Iface | None:
        return next((i for i in self.ifaces if i.name == name), None)

    def primary(self) -> Iface | None:
        return next((i for i in self.ifaces if i.primary), None)

    def summary(self) -> list[str]:
        lines = []
        for i in self.ifaces:
            flags = ", ".join(x for x in (i.kind, "link up" if i.link else "link down",
                                          "PRIMARY (default route)" if i.primary else "") if x)
            lines.append(f"{i.name}: {i.addr}/{i.prefix} gw {i.gateway} metric {i.metric} [{flags}]"
                         + (f" {i.wifi.describe()}" if i.wifi else ""))
        return lines


def _topic(dbg: DbgClient, name: str, schema: dict) -> dict | None:
    try:
        return topic_value(dbg.call("msg.topic", topic=name), schema)
    except DbgError:
        return None


def read_box(dbg: DbgClient) -> BoxSnapshot:
    """The box's interfaces, addresses, Wi-Fi state and log position."""
    snap = BoxSnapshot(uptime_ms=dbg.call("ping").get("uptime_ms", 0),
                       log_total=dbg.call("log.tail", lines=1, source="programs").get("total", 0))
    listed = _topic(dbg, "system/net/interfaces", INTERFACE_LIST)
    for item in (listed or {}).get("list", []):
        kind, dhcp = item["kind"], item["dhcp"]
        iface = Iface(item["name"], item["mac"], item["link"],
                      NIC_KINDS[kind] if kind < len(NIC_KINDS) else str(kind),
                      DHCP_STATES[dhcp] if dhcp < len(DHCP_STATES) else str(dhcp),
                      item["metric"], item["primary"])
        addr = _topic(dbg, f"system/net/{iface.name}/addr", ADDRESS)
        if addr:
            iface.addr, iface.prefix, iface.gateway = addr["addr"], addr["prefix"], addr["gateway"]
        if iface.kind == "wireless":
            state = _topic(dbg, f"system/net/{iface.name}/wifi/state", WIFI_STATE)
            iface.wifi = WifiLink.from_topic(state) if state else None
        snap.ifaces.append(iface)
    return snap


def check_route(snap: BoxSnapshot, want: str) -> tuple[list[str], list[str]]:
    """`(problems, hints)`: why `want` will not carry the box's new
    connections now, and what the person can do about it."""
    iface = snap.get(want)
    if iface is None:
        return [f"the box has no interface {want} (it has {[i.name for i in snap.ifaces]})"], []
    problems, hints = [], []
    if not iface.link:
        problems.append(f"{want}: link is down")
    if not iface.has_addr:
        problems.append(f"{want}: no IPv4 address yet")
    if not iface.primary:
        top = snap.primary()
        problems.append(f"{want} does not hold the default route"
                        + (f"; {top.name} does (metric {top.metric} against {iface.metric})" if top else ""))
        if top is not None and top.kind == "wired":
            hints.append(f"unplug the Ethernet cable of {top.name}: a wired route (metric {top.metric}) "
                         f"always beats Wi-Fi (metric {iface.metric})")
    return problems, hints


# -- the guest client's output ------------------------------------------------

MARKER = re.compile(r"NETBULK:(native|linux):(PUT|GET):(PASS|FAIL)(.*)")
EARLY_FAIL = re.compile(r"NETBULK:(native|linux):FAIL:(.*)")
DONE = re.compile(r"NETBULK:(native|linux):done ok=(true|false)")


@dataclass
class GuestResult:
    """One `NETBULK:<path>:<PUT|GET>:...` line."""

    pos: int
    path: str
    kind: str
    verdict: str
    bytes: int = 0
    ms: int = 0
    mbps: float = 0.0
    detail: str = ""


@dataclass
class GuestDone:
    pos: int
    path: str
    ok: bool


def parse_guest_line(text: str, pos: int = 0) -> GuestResult | GuestDone | None:
    """A `NETBULK:` line as a result, the closing `done` line, or None."""
    if (m := DONE.search(text)):
        return GuestDone(pos, m[1], m[2] == "true")
    if (m := MARKER.search(text)):
        fields = dict(re.findall(r"(\w+)=(\S+)", m[4]))
        try:
            return GuestResult(pos, m[1], m[2], m[3], int(fields.get("bytes", 0)), int(fields.get("ms", 0)),
                               float(fields.get("mbps", 0)), m[4].strip(": "))
        except ValueError:
            return GuestResult(pos, m[1], m[2], "FAIL", detail=f"unreadable: {m[4].strip()}")
    if (m := EARLY_FAIL.search(text)):
        return GuestResult(pos, m[1], "", "FAIL", detail=m[2])
    return None


#: Where the person's command leaves its output. A desktop Terminal reports
#: only the first line of a command's output to the log `dbgd` reads (one
#: `TERM:OUT:` per command), so the full output goes to a file `dbgd` may
#: read (`/tmp` is on its list, `libs/dbgwire/src/fsallow.rs`).
OUT_FILE = "/tmp/netbulk.out"  # the default of `netbulk_command`; lan_bulk.py picks a name per run


def netbulk_command(ip: str, port: int, size: int, seed: int, rounds: int,
                    out_file: str | None = OUT_FILE) -> str:
    """The line typed at the box's Terminal."""
    base = f"netbulk {ip} {port} {size} {seed} {rounds}"
    return f"{base} > {out_file}; cat {out_file}" if out_file else base


def parse_output(text: str) -> tuple[list[GuestResult], list[GuestDone]]:
    """The results and `done` lines in `netbulk`'s output (lines in order)."""
    results: list[GuestResult] = []
    done: list[GuestDone] = []
    for index, raw in enumerate(text.splitlines()):
        parsed = parse_guest_line(raw, index)
        if isinstance(parsed, GuestResult):
            results.append(parsed)
        elif isinstance(parsed, GuestDone):
            done.append(parsed)
    return results, done


def read_out_file(dbg: DbgClient, path: str = OUT_FILE) -> str | None:
    """The output file's text, or None when it does not exist (yet)."""
    try:
        answer = dbg.call("fs.read", path=path, len=8192)
    except DbgError:
        return None
    return answer.get("text") if answer.get("kind") == "file" else None


class GuestLog:
    """Collects the box's `NETBULK:` lines from its programs log.

    `dbgd` serves one client at a time (a second one hangs until the first
    closes), so there is no streaming connection of its own: whoever holds the
    connection calls `poll`, which reads the newest lines of the ring. Lines
    older than `baseline` (the ring's size before this run) are ignored, and
    each is taken once by its position.
    """

    #: Lines read per poll; the box's own audit lines and ticker output share the ring.
    TAIL = 2000

    def __init__(self, baseline: int = 0):
        self.baseline = baseline
        self.results: list[GuestResult] = []
        self.done: list[GuestDone] = []
        self._seen: set[int] = set()

    def poll(self, dbg: DbgClient) -> None:
        self._take(dbg.call("log.tail", lines=self.TAIL, source="programs").get("lines", []))

    def snapshot(self) -> tuple[list[GuestResult], list[GuestDone]]:
        return sorted(self.results, key=lambda r: r.pos), sorted(self.done, key=lambda d: d.pos)

    def _take(self, records: list[dict]) -> None:
        for record in records:
            pos = record.get("pos", -1)
            if pos < self.baseline or pos in self._seen or "NETBULK:" not in record.get("text", ""):
                continue
            self._seen.add(pos)
            parsed = parse_guest_line(record["text"], pos)
            if isinstance(parsed, GuestResult):
                self.results.append(parsed)
            elif isinstance(parsed, GuestDone):
                self.done.append(parsed)


def wait_until(predicate, timeout: float, step: float = 0.5) -> bool:
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        if predicate():
            return True
        time.sleep(step)
    return predicate()
