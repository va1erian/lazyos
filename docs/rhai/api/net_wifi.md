# `sys::net_wifi`

The Rhai module for the Messenger interface `os.lazy.net.wifi.v1`. Wire-level reference: [os.lazy.net.wifi.v1](../../idl/os.lazy.net.wifi.v1.md).

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

**Secrets.** A passphrase is accepted on exactly one call, `AddNetwork`
with scope `User`; `wlanmd` passes it to `keyd` at once
(`StoreSecretFor`), keeps nothing and never logs it, and no method ever
returns one. A system network's passphrase never comes here: `elevd`
stores it in `keyd` itself (`net.wifi.system`, under the profile id),
and `keyd` refuses `wlanmd` a `system` secret. Everything else here is non-secret: SSIDs,
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

## Functions

### `interfaces()`

`Interfaces() -> (names: Array<String>)`

The wireless interfaces `wlanmd` manages, by name (`wlan0`).

Returns `names`: `Array<String>` (array of string).

```rhai
let result = sys::net_wifi::interfaces();
```

### `scan(ifname, ssids)`

`Scan(ifname: String, ssids: Array<String>) -> ()`

Ask for a fresh scan; returns at once, the results arrive on
`system/net/{ifname}/wifi/scan`. `ssids` are hidden networks to probe
for (at most 4); empty is a wildcard scan. A scan is refused while one
is under way (`EBUSY`) and may be skipped while the link is busy with
the handshake.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `ifname` | `String` | string |
| `ssids` | `Array<String>` | array of string |

Returns `()`.

```rhai
let result = sys::net_wifi::scan(ifname, ssids);
```

### `networks(ifname)`

`Networks(ifname: String) -> (list: Array<Network>)`

The current scan results merged with the known profiles: one entry per
network (SSID and security), strongest BSS first, with the profile id
when the caller may use a known network. Profiles of other users are
not shown. At most 128 entries.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `ifname` | `String` | string |

Returns `list`: `Array<Network>` (array of map (`Network`)).

```rhai
let result = sys::net_wifi::networks(ifname);
```

### `add_network(profile, passphrase)`

`AddNetwork(profile: Profile, passphrase: Option<String>) -> (id: String)`

Remember a network; `scope` is a `Scope` ordinal. Scope `User`: the
caller's network and its secret; `profile.id` must be empty (the
service picks one and returns it), and the passphrase is required for
`Wpa2Psk` (8 to 63 octets of UTF-8 without control characters, or
exactly 64 hex digits for a raw PSK) and absent for `Open`. Scope
`System` (from `elevd` alone): only the profile, under the id
`profile.id` names (`w` and 12 lowercase hex digits), whose secret
`elevd` already stored in `keyd` under that id; the passphrase must be
absent. Does not connect. `EEXIST` for the same SSID and scope (or the
same id); `EINVAL` for a bad SSID (1 to 32 bytes), id, security or
passphrase, or a name that matches several heard networks; `EPERM` for
scope `System` from anyone but `elevd`; `ENOSPC` past 32 networks for
one owner.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `profile` | `Profile` | map (`Profile`) |
| `passphrase` | `Option<String>` | string or () |

Returns `id`: `String` (string).

```rhai
let result = sys::net_wifi::add_network(profile, passphrase);
```

### `connect(ifname, id)`

`Connect(ifname: String, id: String) -> ()`

Join the network `id` on `ifname`: leave the current one, scan if the
SSID is not in the results, pick the best BSS, authenticate, associate
and run the handshake. Returns when the attempt is started; the
outcome is on `system/net/{ifname}/wifi/state` (`Connected` or
`Failed` with a reason). A user network belongs to its owner: another
user gets `EPERM`.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `ifname` | `String` | string |
| `id` | `String` | string |

Returns `()`.

```rhai
let result = sys::net_wifi::connect(ifname, id);
```

### `disconnect(ifname)`

`Disconnect(ifname: String) -> ()`

Leave the current network and do not reconnect until asked (auto-
connect is suspended for this interface until the next `Connect`).
Not an error when not connected.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `ifname` | `String` | string |

Returns `()`.

```rhai
let result = sys::net_wifi::disconnect(ifname);
```

### `forget(id)`

`Forget(id: String) -> ()`

Delete a profile and its secret. Disconnects first if it is the
current network. Not an error for an id that is gone.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `id` | `String` | string |

Returns `()`.

```rhai
let result = sys::net_wifi::forget(id);
```

### `status(ifname)`

`Status(ifname: String) -> (status: WifiState)`

The interface's state now: the same value the retained `state` topic
carries, for a client that does not subscribe.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `ifname` | `String` | string |

Returns `status`: `WifiState` (map (`WifiState`)).

```rhai
let result = sys::net_wifi::status(ifname);
```

## Types

### `Network`

A heard network, merged with what is known about it.

| Name | IDL type | Script value |
|---|---|---|
| `ssid` | `String` | string |
| `security` | `U32` | int |
| `rssi_dbm` | `I32` | int |
| `freq_mhz` | `U32` | int |
| `bss_count` | `U32` | int |
| `id` | `String` | string |
| `connected` | `Bool` | bool |

```rhai
let value = sys::net_wifi::new_network();
```

### `Profile`

UTF-8 text sanitised by the service, at most 32 bytes. Empty for a
hidden network that is not a known profile (shown by BSSID only).
A `Security` ordinal.
Signal of the strongest BSS in dBm.
Frequency in MHz of that BSS.
How many BSSes of this network were heard.
The profile id when this is a known network the caller may use;
empty otherwise.
Whether this is the network the interface is connected to.
A network to remember. Non-secret fields only (the passphrase travels
as `AddNetwork`'s own argument).

| Name | IDL type | Script value |
|---|---|---|
| `id` | `String` | string |
| `ssid` | `String` | string |
| `security` | `U32` | int |
| `hidden` | `Bool` | bool |
| `autoconnect` | `Bool` | bool |
| `priority` | `U32` | int |
| `scope` | `U32` | int |

```rhai
let value = sys::net_wifi::new_profile();
```

### `WifiState`

Empty on `AddNetwork`; set when listed.
A `Security` ordinal.
Probe for the SSID even when no beacon shows it.
Join it automatically when in range.
Higher wins when several known networks are in range.
A `Scope` ordinal.
An interface's connection state: the payload of
`system/net/{ifname}/wifi/state`.

| Name | IDL type | Script value |
|---|---|---|
| `state` | `U32` | int |
| `ssid` | `String` | string |
| `bssid` | `Bytes` | blob |
| `rssi_dbm` | `I32` | int |
| `freq_mhz` | `U32` | int |
| `id` | `String` | string |
| `reason` | `U32` | int |
| `changes` | `U32` | int |

```rhai
let value = sys::net_wifi::new_wifi_state();
```

### `ScanEvent`

A `ConnState` ordinal.
The network joined or being joined (empty when none).
Six octets, the BSSID in use (empty when none).
Signal of the current BSS in dBm (0 when not connected).
The profile id in use (empty when none).
A `FailReason` ordinal; meaningful when `state` is `Failed`,
`None` otherwise.
Changes since the service started, so a subscriber can tell a
repeat from a flap.
A scan finished: the payload of `system/net/{ifname}/wifi/scan`. It
carries no networks (those come from `Networks`, which is filtered
per caller); it only says the list changed.

| Name | IDL type | Script value |
|---|---|---|
| `generation` | `U32` | int |
| `count` | `U32` | int |

```rhai
let value = sys::net_wifi::new_scan_event();
```

### `Security`

A string; the constants of this module are:

- `SECURITY_OPEN` = `"Open"`
- `SECURITY_WPA2_PSK` = `"Wpa2Psk"`
- `SECURITY_UNSUPPORTED` = `"Unsupported"`

`SECURITY` is the array of all of them.

### `Scope`

A string; the constants of this module are:

- `SCOPE_USER` = `"User"`
- `SCOPE_SYSTEM` = `"System"`

`SCOPE` is the array of all of them.

### `ConnState`

A string; the constants of this module are:

- `CONN_STATE_DISABLED` = `"Disabled"`
- `CONN_STATE_IDLE` = `"Idle"`
- `CONN_STATE_SCANNING` = `"Scanning"`
- `CONN_STATE_AUTHENTICATING` = `"Authenticating"`
- `CONN_STATE_ASSOCIATING` = `"Associating"`
- `CONN_STATE_HANDSHAKE` = `"Handshake"`
- `CONN_STATE_CONNECTED` = `"Connected"`
- `CONN_STATE_FAILED` = `"Failed"`

`CONN_STATE` is the array of all of them.

### `FailReason`

A string; the constants of this module are:

- `FAIL_REASON_NONE` = `"None"`
- `FAIL_REASON_NOT_FOUND` = `"NotFound"`
- `FAIL_REASON_REJECTED` = `"Rejected"`
- `FAIL_REASON_BAD_PASSPHRASE` = `"BadPassphrase"`
- `FAIL_REASON_TIMEOUT` = `"Timeout"`
- `FAIL_REASON_DEAUTHENTICATED` = `"Deauthenticated"`
- `FAIL_REASON_BEACON_LOSS` = `"BeaconLoss"`
- `FAIL_REASON_POLICY_MISMATCH` = `"PolicyMismatch"`
- `FAIL_REASON_RADIO` = `"Radio"`
- `FAIL_REASON_UNSUPPORTED` = `"Unsupported"`

`FAIL_REASON` is the array of all of them.

## Topics

### `system/net/{ifname}/wifi/state`

Scans completed since the service started.
Networks in the merged list.
The interface's state, retained so a late subscriber (the tray applet
after login) sees it at once. `{ifname}` is the radio's interface name.

Payload `WifiState`, QoS `latest`, retained.

Helpers: `STATE_PATTERN`, `state_topic(ifname)`, `on_state(ifname, handler)`, `subscribe_state(ifname)`, `publish_state(ifname, payload)`.

```rhai
sys::net_wifi::on_state(ifname, |event| print(event.payload));
```

### `system/net/{ifname}/wifi/scan`

A scan completed and `Networks` changed. Not retained: a client that
arrives later calls `Networks`.

Payload `ScanEvent`, QoS `latest`.

Helpers: `SCAN_PATTERN`, `scan_topic(ifname)`, `on_scan(ifname, handler)`, `subscribe_scan(ifname)`, `publish_scan(ifname, payload)`.

```rhai
sys::net_wifi::on_scan(ifname, |event| print(event.payload));
```

## See also

- [Messenger APIs for Rhai](index.md)
- [msg: Messenger from Rhai](../msg.md)
- [os.lazy.net.wifi.v1 on the wire](../../idl/os.lazy.net.wifi.v1.md)
