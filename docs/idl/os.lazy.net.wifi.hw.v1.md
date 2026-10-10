# `os.lazy.net.wifi.hw.v1`

Interface id: `0xa882dc6295ff08bd`

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
driver serves to `netd` (the chip does the 802.3 to 802.11 conversion and
the encryption). The NIC's link state follows this interface: it is down
until `SetState(Authorized)`. A driver whose chip cannot convert frames is
out of scope for v1.

**One client.** The first caller of `Attach` owns the radio; its identity
is the kernel-stamped sender, and every later call or `Detach` by anyone
else fails with `EACCES` (a second attach with `EBUSY`). The owner's exit,
or its event channel reporting the peer gone, detaches it and leaves the
radio idle: scanning stops, the BSS is left, all keys are deleted.

**Events.** `Attach` transfers the client's event channel; the driver sends
the `oneway` methods below (`ScanDone`, `RxMgmt`, `BeaconLoss`,
`Deauthenticated`) on it. They carry the same bounds as calls: a frame is
at most `HwInfo.max_mgmt` bytes, never truncated and never delivered
partially; one that does not fit is dropped and counted.

**Keys cross this interface.** `SetKey` is the one place secret material
(a temporal key) leaves `wlanmd`: the hardware needs it to encrypt. It is
accepted only from the owner, the driver never logs or stores it beyond
the key slot, and it is cleared by `DelKey`, `Leave` and `Detach`.

Failures of calls are returned as the shared structured error field
(`services::error_field`) instead of the declared reply fields.

## Methods

| Method | Id | Kind | Signature |
|---|---|---|---|
| Info | 266462757 | sync | `() -> (info: HwInfo)` |
| Attach | 145305188 | sync | `() -> () transfers (events: Channel<os.lazy.net.wifi.hw.v1>)` |
| Detach | 475812562 | sync | `() -> ()` |
| SetCountry | 820455787 | sync | `(alpha2: String) -> ()` |
| Scan | 1830061320 | sync | `(request: ScanRequest) -> (scan_id: U32)` |
| AbortScan | 1659626708 | sync | `(scan_id: U32) -> ()` |
| ScanResults | 536623814 | sync | `(scan_id: U32, first: U32, max: U32) -> (results: Array<ScanEntry>, more: Bool)` |
| Join | 805458841 | sync | `(request: JoinRequest) -> ()` |
| TxMgmt | 583187128 | sync | `(kind: U32, frame: Bytes) -> ()` |
| SetState | 1345327380 | sync | `(state: U32, aid: U32) -> ()` |
| SetKey | 1206977594 | sync | `(kind: U32, index: U32, cipher: U32, key: Bytes, rsc: Bytes, addr: Bytes) -> ()` |
| DelKey | 36584897 | sync | `(kind: U32, index: U32) -> ()` |
| Leave | 2049913440 | sync | `(reason: U32) -> ()` |
| Stats | 267161228 | sync | `() -> (stats: HwStats)` |
| ScanDone | 2037373080 | oneway | `(scan_id: U32, aborted: Bool) -> ()` |
| RxMgmt | 1961753042 | oneway | `(kind: U32, rssi_dbm: I32, frame: Bytes) -> ()` |
| BeaconLoss | 683463118 | oneway | `() -> ()` |
| Deauthenticated | 1135744209 | oneway | `(reason: U32) -> ()` |

## Transfers

Objects a request carries outside its body, in the parcel's
`handles` and `buffers` vectors.

| Method | Name | Slot |
|---|---|---|
| Attach | `events` | `handles[0]`, a channel the receiver sends `os.lazy.net.wifi.hw.v1` on |

## struct `HwInfo`

- `mac: Bytes`
- `interface: String`
- `bands: U32`
- `ciphers: U32`
- `scan_offload: Bool`
- `max_mgmt: U32`
- `max_scan_ssids: U32`
- `max_scan_channels: U32`
- `key_slots: U32`
- `features: U32`

## struct `ScanRequest`

- `channels: Array<U32>`
- `band: U32`
- `ssids: Array<Bytes>`
- `active: Bool`
- `dwell_ms: U32`

## struct `ScanEntry`

- `frame: Bytes`
- `channel: U32`
- `band: U32`
- `rssi_dbm: I32`
- `age_ms: U32`

## struct `JoinRequest`

- `bssid: Bytes`
- `channel: U32`
- `band: U32`
- `width: U32`

## struct `HwStats`

- `scans: U64`
- `mgmt_rx: U64`
- `mgmt_tx: U64`
- `mgmt_dropped: U64`
- `beacon_losses: U64`
- `deauths: U64`
- `fw_errors: U64`
- `resets: U64`

## enum `FrameKind`

- Mgmt, Eapol

## enum `Band`

- Ghz2, Ghz5, Ghz6

## enum `Width`

- Mhz20, Mhz40, Mhz80, Mhz160

## enum `Cipher`

- Ccmp128, Gcmp256, BipCmac128

## enum `KeyKind`

- Pairwise, Group, Igtk

## enum `StaState`

- Idle, Authenticated, Associated, Authorized
