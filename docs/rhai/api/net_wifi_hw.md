# `sys::net_wifi_hw`

The Rhai module for the Messenger interface `os.lazy.net.wifi.hw.v1`. Wire-level reference: [os.lazy.net.wifi.hw.v1](../../idl/os.lazy.net.wifi.hw.v1.md).

A Wi-Fi radio, **seen from the station manager** (docs/wifi-prerequisites-plan.md
section 3.5, docs/wifi-plan.md section 5.1). The chip driver (`wifid`, uid
`_wifi`, holds `CAP_DEV_CLAIM` and the bus) serves it; its one client is
`wlanmd` (uid `_wlan`, no capabilities). The simulator `wifisim` (`_wifisim`)
serves the same interface in CI and is its first implementation, which is
why nothing here is shaped by one chip.

**Trust.** The side that holds DMA parses nothing from the air and the side
that parses the air holds no capabilities. So this interface moves
*opaque management frames* in both directions: the driver hands over raw
beacon, probe-response, authentication and association bodies exactly as
the radio received them (with the radio's own metadata: channel, signal,
age) and transmits frames `wlanmd` built, and it never looks inside an
information element. Everything the driver does decide is a hardware
matter: which channels it may tune (it enforces the country it was
given), key slots, and the association state its firmware needs.

**The data path is not here.** Once the station is authorized, payload
frames travel as 802.3 Ethernet over `os.lazy.net.nic.v1`, which the same
driver serves to `netd`. The 802.3 to 802.11 conversion happens below this
interface: in the chip's firmware where it offers that, else in the
driver's soft-MAC layer (`libs/wifimac`, docs/wifi-rtw88-plan.md), as on
the RTL8822CE. The NIC's link state follows this interface: it is down
until `SetState(Authorized)`.

**Names.** A radio serves this interface as `os.lazy.net.wifi.hw/<ifname>`,
`<ifname>` being the name of its `os.lazy.net.nic.v1` card (`wlan0`, which
it serves as `os.lazy.net.nic/wlan0`). The registry lets only the Wi-Fi
driver identities (`_wifi`, `_wifisim`) hold names there
(`netpolicy::may_register_wifi_hw_name`); `wlanmd` finds radios by listing
that namespace.

**One client.** The first caller of `Attach` owns the radio; its identity
is the kernel-stamped sender, its task and its credentials (uid, label,
session), all of which must match on every later call, and a call or
`Detach` by anyone else fails with `EACCES` (a second attach with
`EBUSY`). `Attach` is accepted from `_wlan` alone, unlabelled and outside
any session (`netpolicy::may_attach_wifi_hw`; `EPERM` otherwise), so
nobody can take the radio before `wlanmd` does; the simulator also lets
its own test client (`wifiprobe`, running as `_wifisim`) attach. The
owner's exit, or its event channel reporting the peer gone, detaches it
and leaves the radio idle: scanning stops, the BSS is left, all keys are
deleted.

**Events.** `Attach` carries the client's event channel; the driver sends
the `oneway` methods below (`ScanDone`, `RxMgmt`, `BeaconLoss`,
`Deauthenticated`) on it. They carry the same bounds as calls: a frame is
at most `HwInfo.max_mgmt` bytes, never truncated and never delivered
partially; one that does not fit is dropped and counted. Anyone in radio
range can forge frames from the BSS, so `RxMgmt` is rate-limited and is
the first thing dropped when the client falls behind: `ScanDone`,
`BeaconLoss` and `Deauthenticated` have room of their own and are kept
until the client's inbox takes them (a repeat of a `BeaconLoss` or
`Deauthenticated` the client has not read yet is folded into it). What is
dropped is counted in `HwStats.mgmt_dropped`.

**Keys cross this interface.** `SetKey` is the one place secret material
(a temporal key) leaves `wlanmd`: the hardware needs it to encrypt. It is
accepted only from the owner, the driver never logs or stores it beyond
the key slot, and it is cleared by `DelKey`, `Leave` and `Detach`.

Failures of calls are returned as the shared structured error field
(`services::error_field`) instead of the declared reply fields.

## Functions

### `info()`

`Info() -> (info: HwInfo)`

Describe the radio: addresses, bands, ciphers, limits. Any caller the
label policy lets reach the driver may ask, attached or not: it reveals
only hardware facts.

Returns `info`: `HwInfo` (map (`HwInfo`)).

```rhai
let result = sys::net_wifi_hw::info();
```

### `detach()`

`Detach() -> ()`

Release the radio (also implied when the owner exits). Leaves the BSS,
aborts a scan and deletes every key.

Returns `()`.

```rhai
let result = sys::net_wifi_hw::detach();
```

### `set_country(alpha2)`

`SetCountry(alpha2: String) -> ()`

Tell the radio which regulatory domain applies: an ISO 3166 alpha-2
code, or `00` for the world domain (passive scanning only on channels
where transmitting first is not universally allowed, no 6 GHz). The
driver must not transmit on a channel the domain forbids and may refuse
to transmit at all before the first call (`EAGAIN` from `Scan` with
active probing, `Join`, `TxMgmt`). A code that is not an assigned ISO
3166-1 code (`ZZ`) is `EINVAL`. A new domain applies at once: a running
scan skips the channels it forbids and probes only where it allows,
and a joined BSS on a channel it forbids is left without transmitting
and reported as `Deauthenticated` (reason 3). The value comes from
`wlanmd`, which reads `sys/net/wifi/country`; the driver trusts nobody
else for it.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `alpha2` | `String` | string |

Returns `()`.

```rhai
let result = sys::net_wifi_hw::set_country(alpha2);
```

### `scan(request)`

`Scan(request: ScanRequest) -> (scan_id: U32)`

Start a scan. The reply is the id `ScanDone` and `ScanResults` name; it
does not wait. Only one scan runs at a time (`EBUSY`). At most
`HwInfo.max_scan_ssids` SSIDs of at most 32 bytes and at most
`HwInfo.max_scan_channels` channels (`EINVAL` beyond, or for a channel
the radio or its domain does not have). A scan during an association
is allowed and pauses traffic while the radio is on other channels:
nothing is sent to the BSS from another channel (see `TxMgmt`).

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `request` | `ScanRequest` | map (`ScanRequest`) |

Returns `scan_id`: `U32` (int).

```rhai
let result = sys::net_wifi_hw::scan(request);
```

### `abort_scan(scan_id)`

`AbortScan(scan_id: U32) -> ()`

Stop the running scan early; `ScanDone` follows with `aborted` set.
Not an error when none runs.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `scan_id` | `U32` | int |

Returns `()`.

```rhai
let result = sys::net_wifi_hw::abort_scan(scan_id);
```

### `scan_results(scan_id, first, max)`

`ScanResults(scan_id: U32, first: U32, max: U32) -> (results: Array<ScanEntry>, more: Bool)`

One page of a finished (or running) scan's results, in the order the
radio heard them, from entry `first`; at most `max` entries (capped at
64). `more` says whether a later page holds entries. Each entry is the
raw management frame, **unparsed**: parsing it is `wlanmd`'s job.
An unknown `scan_id` is `ENOENT`.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `scan_id` | `U32` | int |
| `first` | `U32` | int |
| `max` | `U32` | int |

Returns a map:

| Name | IDL type | Script value |
|---|---|---|
| `results` | `Array<ScanEntry>` | array of map (`ScanEntry`) |
| `more` | `Bool` | bool |

```rhai
let result = sys::net_wifi_hw::scan_results(scan_id, first, max);
```

### `join(request)`

`Join(request: JoinRequest) -> ()`

Tune to the BSS's channel and filter to its address so `TxMgmt` and
`RxMgmt` carry the authentication and association exchange. Leaves any
previous BSS first. The reply means the radio is on the channel, not
that anything was heard.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `request` | `JoinRequest` | map (`JoinRequest`) |

Returns `()`.

```rhai
let result = sys::net_wifi_hw::join(request);
```

### `tx_mgmt(kind, frame)`

`TxMgmt(kind: U32, frame: Bytes) -> ()`

Send a management or EAPOL frame `wlanmd` built, as is. `kind` is a
`FrameKind` ordinal; for `Mgmt` the frame is a complete 802.11
management frame without FCS, for `Eapol` a complete 802.3 EAPOL frame
(the radio adds the 802.11 header and, once keys are installed, the
encryption). Only while joined (`ENOTCONN`); a frame longer than
`HwInfo.max_mgmt` (or, as EAPOL, too long for the radio once
wrapped) is `EINVAL`. A management frame is from the station to the
joined BSS (receiver and BSSID, or broadcast in either for a probe
request), an EAPOL frame to the BSSID; anything else is `EINVAL`. The
reply means queued, not delivered: while a scan has the radio on
another channel, or on a passive channel before the BSS was heard
there, the frame waits (`EBUSY` when too many already wait), and a
full transmit queue is `EBUSY`. A frame goes out under the keys in
force when it was queued: message 4 of the 4-way handshake is queued
just before the pairwise key is installed and must leave unencrypted.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `kind` | `U32` | int |
| `frame` | `Bytes` | blob |

Returns `()`.

```rhai
let result = sys::net_wifi_hw::tx_mgmt(kind, frame);
```

### `set_state(state, aid)`

`SetState(state: U32, aid: U32) -> ()`

Move the radio's association state: a `StaState` ordinal. `Associated`
carries the association id the AP assigned; `Authorized` opens the
controlled port (data frames flow, the NIC's link comes up) and is
valid only from `Associated`, and only after the pairwise key is
installed, or straight after `Associated` for an open network (no key
at all; a BSS whose scan entry or beacons advertised privacy or an RSN
element is never open, `EINVAL`). Going to a lower state is always
allowed and takes the link down; below `Associated` it deletes every
key.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `state` | `U32` | int |
| `aid` | `U32` | int |

Returns `()`.

```rhai
let result = sys::net_wifi_hw::set_state(state, aid);
```

### `set_key(kind, index, cipher, key, rsc, addr)`

`SetKey(kind: U32, index: U32, cipher: U32, key: Bytes, rsc: Bytes, addr: Bytes) -> ()`

Install a key in a hardware slot. `kind` is a `KeyKind` ordinal,
`index` the key id (pairwise 0, group 1 to 3, IGTK 4 to 5), `cipher`
a `Cipher` ordinal the radio advertised, `key` its exact length,
`rsc` the receive sequence counter the handshake delivered (little
endian, up to 8 bytes) and `addr` the peer address (the BSSID for a
group key). Replaces a key in the same slot atomically. The transmit
packet number belongs to the association, not the key: a new or
reinstalled pairwise key continues it, and only leaving the BSS starts
it over. `EINVAL` for a length or index that does not fit, `ENOSPC`
for no free slot; when the radio refuses a key, the slot is left
empty.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `kind` | `U32` | int |
| `index` | `U32` | int |
| `cipher` | `U32` | int |
| `key` | `Bytes` | blob |
| `rsc` | `Bytes` | blob |
| `addr` | `Bytes` | blob |

Returns `()`.

```rhai
let result = sys::net_wifi_hw::set_key(kind, index, cipher, key, rsc, addr);
```

### `del_key(kind, index)`

`DelKey(kind: U32, index: U32) -> ()`

Remove a key; not an error when the slot is empty.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `kind` | `U32` | int |
| `index` | `U32` | int |

Returns `()`.

```rhai
let result = sys::net_wifi_hw::del_key(kind, index);
```

### `leave(reason)`

`Leave(reason: U32) -> ()`

Leave the BSS: a `Deauthentication` with this IEEE 802.11 reason code
is sent if the radio can, every key is deleted, the NIC's link goes
down and the radio returns to idle. Not an error when not joined.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `reason` | `U32` | int |

Returns `()`.

```rhai
let result = sys::net_wifi_hw::leave(reason);
```

### `stats()`

`Stats() -> (stats: HwStats)`

Counters since the driver started.

Returns `stats`: `HwStats` (map (`HwStats`)).

```rhai
let result = sys::net_wifi_hw::stats();
```

### `scan_done(scan_id, aborted)`

`ScanDone(scan_id: U32, aborted: Bool) -> () oneway`

Event, driver to client: the scan finished (`aborted` false) or was
stopped (`aborted` true, or the radio could not finish); its results
are readable with `ScanResults` until the next `Scan`.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `scan_id` | `U32` | int |
| `aborted` | `Bool` | bool |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::net_wifi_hw::scan_done(scan_id, aborted);
```

### `rx_mgmt(kind, rssi_dbm, frame)`

`RxMgmt(kind: U32, rssi_dbm: I32, frame: Bytes) -> () oneway`

Event, driver to client: a management or EAPOL frame addressed to this
station arrived from the joined BSS, and the client can be waiting for
it: an authentication or (re)association response only after
`TxMgmt` sent its request, an action frame once `Associated`, EAPOL
once the AP answered the association; a retransmission of the last
unprotected frame is dropped. `kind` is a `FrameKind` ordinal,
`rssi_dbm` the signal in dBm. The frame has the shape `TxMgmt` takes:
a whole management frame (from the MAC header, no FCS), or EAPOL as
an 802.3 frame (destination, source, ethertype 0x888E, then the
EAPOL PDU), decrypted. The bytes are unparsed and hostile.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `kind` | `U32` | int |
| `rssi_dbm` | `I32` | int |
| `frame` | `Bytes` | blob |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::net_wifi_hw::rx_mgmt(kind, rssi_dbm, frame);
```

### `beacon_loss()`

`BeaconLoss() -> () oneway`

Event, driver to client: the radio stopped hearing the AP (the
firmware's missed-beacon threshold). The client decides what to do.

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::net_wifi_hw::beacon_loss();
```

### `deauthenticated(reason)`

`Deauthenticated(reason: U32) -> () oneway`

Event, driver to client: the AP deauthenticated or disassociated us.
`reason` is the IEEE 802.11 reason code, taken from the frame by the
firmware (the one field of a frame the driver reports; the frame
itself is also delivered as `RxMgmt`).

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `reason` | `U32` | int |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::net_wifi_hw::deauthenticated(reason);
```

Not callable from a script (the request carries a kernel object): `Attach`.

## Types

### `HwInfo`

What the radio is.

| Name | IDL type | Script value |
|---|---|---|
| `mac` | `Bytes` | blob |
| `interface` | `String` | string |
| `bands` | `U32` | int |
| `ciphers` | `U32` | int |
| `scan_offload` | `Bool` | bool |
| `max_mgmt` | `U32` | int |
| `max_scan_ssids` | `U32` | int |
| `max_scan_channels` | `U32` | int |
| `key_slots` | `U32` | int |
| `features` | `U32` | int |

```rhai
let value = sys::net_wifi_hw::new_hw_info();
```

### `ScanRequest`

Six octets, network order: the station address (also the NIC's).
The registry name of the radio's `os.lazy.net.nic.v1` interface
(`wlan0`): how `wlanmd` names the interface in topics.
Bands, a `Band` bitmap: bit *n* for the `Band` with ordinal *n*.
Ciphers the hardware can run, a `Cipher` bitmap.
Whether the firmware scans on its own (offload) rather than being
stepped by the host.
Largest management frame in either direction, in bytes.
Most SSIDs one scan may probe for.
Most channels one scan may name.
Hardware key slots (pairwise and group together).
Feature bits, zero if none. Reserved bits are zero; a client
ignores them.
What to scan. Channel numbers are 802.11 channel numbers within a
band, with `band` selecting which.

| Name | IDL type | Script value |
|---|---|---|
| `channels` | `Array<U32>` | array of int |
| `band` | `U32` | int |
| `ssids` | `Array<Bytes>` | array of blob |
| `active` | `Bool` | bool |
| `dwell_ms` | `U32` | int |

```rhai
let value = sys::net_wifi_hw::new_scan_request();
```

### `ScanEntry`

Channels to visit; empty means every channel the radio and the
regulatory domain allow.
A `Band` ordinal, used when `channels` is empty to limit the sweep
to one band; `U32::MAX` means all bands.
SSIDs to probe for (hidden networks); empty is a wildcard probe.
Probe actively where the domain allows; otherwise listen only.
Milliseconds to stay on each channel, 0 for the driver's default.
One heard frame: a beacon or probe response, unparsed.

| Name | IDL type | Script value |
|---|---|---|
| `frame` | `Bytes` | blob |
| `channel` | `U32` | int |
| `band` | `U32` | int |
| `rssi_dbm` | `I32` | int |
| `age_ms` | `U32` | int |

```rhai
let value = sys::net_wifi_hw::new_scan_entry();
```

### `JoinRequest`

The complete management frame, from the MAC header (the BSSID is
there), without FCS, as received.
The channel it was heard on (a number within `band`).
A `Band` ordinal.
Signal strength in dBm as the radio measured it.
Milliseconds before the results were read that it was heard.
Where to go.

| Name | IDL type | Script value |
|---|---|---|
| `bssid` | `Bytes` | blob |
| `channel` | `U32` | int |
| `band` | `U32` | int |
| `width` | `U32` | int |

```rhai
let value = sys::net_wifi_hw::new_join_request();
```

### `HwStats`

Six octets: the BSSID to authenticate with.
A `Band` ordinal.
A `Width` ordinal: the widest channel the client wants.
Counters since the driver started.

| Name | IDL type | Script value |
|---|---|---|
| `scans` | `U64` | int |
| `mgmt_rx` | `U64` | int |
| `mgmt_tx` | `U64` | int |
| `mgmt_dropped` | `U64` | int |
| `beacon_losses` | `U64` | int |
| `deauths` | `U64` | int |
| `fw_errors` | `U64` | int |
| `resets` | `U64` | int |

```rhai
let value = sys::net_wifi_hw::new_hw_stats();
```

### `FrameKind`

A string; the constants of this module are:

- `FRAME_KIND_MGMT` = `"Mgmt"`
- `FRAME_KIND_EAPOL` = `"Eapol"`

`FRAME_KIND` is the array of all of them.

### `Band`

A string; the constants of this module are:

- `BAND_GHZ2` = `"Ghz2"`
- `BAND_GHZ5` = `"Ghz5"`
- `BAND_GHZ6` = `"Ghz6"`

`BAND` is the array of all of them.

### `Width`

A string; the constants of this module are:

- `WIDTH_MHZ20` = `"Mhz20"`
- `WIDTH_MHZ40` = `"Mhz40"`
- `WIDTH_MHZ80` = `"Mhz80"`
- `WIDTH_MHZ160` = `"Mhz160"`

`WIDTH` is the array of all of them.

### `Cipher`

A string; the constants of this module are:

- `CIPHER_CCMP128` = `"Ccmp128"`
- `CIPHER_GCMP256` = `"Gcmp256"`
- `CIPHER_BIP_CMAC128` = `"BipCmac128"`

`CIPHER` is the array of all of them.

### `KeyKind`

A string; the constants of this module are:

- `KEY_KIND_PAIRWISE` = `"Pairwise"`
- `KEY_KIND_GROUP` = `"Group"`
- `KEY_KIND_IGTK` = `"Igtk"`

`KEY_KIND` is the array of all of them.

### `StaState`

A string; the constants of this module are:

- `STA_STATE_IDLE` = `"Idle"`
- `STA_STATE_AUTHENTICATED` = `"Authenticated"`
- `STA_STATE_ASSOCIATED` = `"Associated"`
- `STA_STATE_AUTHORIZED` = `"Authorized"`

`STA_STATE` is the array of all of them.

## See also

- [Messenger APIs for Rhai](index.md)
- [msg: Messenger from Rhai](../msg.md)
- [os.lazy.net.wifi.hw.v1 on the wire](../../idl/os.lazy.net.wifi.hw.v1.md)
