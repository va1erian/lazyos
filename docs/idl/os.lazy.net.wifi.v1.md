# `os.lazy.net.wifi.v1`

Interface id: `0x515b536f62b1d272`

The Wi-Fi station service, **the system-facing interface** (docs/wifi-prerequisites-plan.md
sections 3.5 and 3.7). `wlanmd` (uid `_wlan`, no capabilities) serves it
under the registry name `os.lazy.net.wifi` and runs the station state
machine over `os.lazy.net.wifi.hw.v1` and `keyd`. Front ends (the Network
app, the tray applet, `wifictl`, `rhai`) are its clients.

**Who may do what** (the caller is the kernel-stamped sender; `elevd` is a
recognised identity, never a claim in a request):

* Reading (`Networks`, `Status`) and `Scan`: any logged-in session user.
* `Connect`, `Disconnect`, `AddNetwork` with scope `User`, and `Forget` of
the caller's own network: any session user, for networks they own.
* `AddNetwork` with scope `System`, `Forget` of a system network: only the
`elevd` identity, after the administrator approved the prompt (action
`net.wifi.system`); a session user gets `EPERM`. `Connect` to a system
network is allowed to anyone (the secret never leaves `keyd`).

**Secrets.** A passphrase is accepted on exactly one call, `AddNetwork`;
`wlanmd` passes it to `keyd` at once, keeps nothing and never logs it, and
no method ever returns one. Everything else here is non-secret: SSIDs,
BSSIDs, security type, signal. A profile is stored in `confd` (system
networks under `sys/net/wifi/networks/<id>`, a user's under
`user/<uid>/net/wifi/networks/<id>`); the secret lives in `keyd` under the
same `<id>` and scope.

**Hostile input.** `Networks` and the scan topic show the result of parsing
beacons that any radio in range can send. Names are length-capped and
stripped of control characters by `wlanmd` before they get here, but a
client still treats an SSID as untrusted text (never as markup, never as a
path or command).

Every method takes the interface name (`wlan0`) the registry gave the
radio; a name `wlanmd` does not manage is `ENODEV`. Failures reply with the
shared structured error field (`services::error_field`).

## Methods

| Method | Id | Kind | Signature |
|---|---|---|---|
| Interfaces | 1779791769 | sync | `() -> (names: Array<String>)` |
| Scan | 1830061320 | sync | `(ifname: String, ssids: Array<String>) -> ()` |
| Networks | 413956514 | sync | `(ifname: String) -> (list: Array<Network>)` |
| AddNetwork | 69972362 | sync | `(profile: Profile, passphrase: Option<String>) -> (id: String)` |
| Connect | 1535748249 | sync | `(ifname: String, id: String) -> ()` |
| Disconnect | 1518631179 | sync | `(ifname: String) -> ()` |
| Forget | 1849666444 | sync | `(id: String) -> ()` |
| Status | 6222351 | sync | `(ifname: String) -> (status: WifiState)` |

## Topics

| Topic | Payload | QoS | Retained | Permissions |
|---|---|---|---|---|
| `system/net/+/wifi/state` | `WifiState` | latest | yes | `publish:system/net/+/wifi/state`, `subscribe:system/net/+/wifi/state` |
| `system/net/+/wifi/scan` | `ScanEvent` | latest | no | `publish:system/net/+/wifi/scan`, `subscribe:system/net/+/wifi/scan` |

## struct `Network`

- `ssid: String`
- `security: U32`
- `rssi_dbm: I32`
- `freq_mhz: U32`
- `bss_count: U32`
- `id: String`
- `connected: Bool`

## struct `Profile`

- `id: String`
- `ssid: String`
- `security: U32`
- `hidden: Bool`
- `autoconnect: Bool`
- `priority: U32`
- `scope: U32`

## struct `WifiState`

- `state: U32`
- `ssid: String`
- `bssid: Bytes`
- `rssi_dbm: I32`
- `freq_mhz: U32`
- `id: String`
- `reason: U32`
- `changes: U32`

## struct `ScanEvent`

- `generation: U32`
- `count: U32`

## enum `Security`

- Open, Wpa2Psk, Unsupported

## enum `Scope`

- User, System

## enum `ConnState`

- Disabled, Idle, Scanning, Authenticating, Associating, Handshake, Connected, Failed

## enum `FailReason`

- None, NotFound, Rejected, BadPassphrase, Timeout, Deauthenticated, BeaconLoss, PolicyMismatch, Radio
