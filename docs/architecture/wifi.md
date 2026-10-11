# Wi-Fi

The plan is [wifi-prerequisites-plan.md](../wifi-prerequisites-plan.md) (the
stack above the driver) and [wifi-plan.md](../wifi-plan.md) (the hardware and
licence). This page describes what is built, stage by stage: WP3 (the protocol libraries) and
WP4's station half (`wlansta`, `wlanmd`, `wifictl`).

## WP3: the 802.11 protocol libraries

Two host-tested, fuzzed, `no_std` + `alloc` libraries with no `unsafe`, used by
the station manager `wlanmd` (WP4) and cross-checked by the simulator's access
point (`tools/wifi/fake_ap.py`, WP4). They never see the data path: the chip
hands the data path to `netd` as Ethernet, and the chip handles the 802.11
data header and CCMP, so only management frames and EAPOL are parsed. Both are
written from IEEE Std 802.11-2020 alone (`THIRD_PARTY.md`).

### `libs/ieee80211`

Everything arrives from an access point anyone can run: every length is
checked, loops are bounded by their input, allocation is bounded by the input
(or a cap), and nothing panics. The behaviour on malformed elements is
documented in the crate docs: a truncated tail keeps what came before it
(`Elements::truncated`), the first of a duplicated singleton element wins
(`duplicates`), a known element of impossible length is ignored (`malformed`),
an RSN element that does not parse makes the BSS `Security::InvalidRsn`, and
an SSID over 32 octets is the one error that refuses the frame.

| Item | What it does |
|---|---|
| `Mgmt::parse(&[u8])` | Header (24 octets, +4 HT Control with the Order bit) and body of a management frame, no FCS. `Body::{Beacon, ProbeRequest, ProbeResponse, Auth, AssocRequest, AssocResponse, Deauth, Disassoc, Action, Other}`. Refuses a version other than 0, non-management types, the Protected bit and fragments |
| `Elements::parse(ies)` | SSID (may be empty, all NULs, non-UTF-8; `hidden()`), rates, DS channel, country, HT/VHT/HE capabilities as raw slices, the first RSN element (body and whole element), the WPA1 vendor element, up to 32 vendor elements |
| `Rsn::{parse_body, parse_ie, to_body, to_ie}` | Version, group cipher, pairwise list, AKM list, capabilities (`RsnCaps`), PMKID list, group management cipher. Optional tail fields default per the standard; counts are checked against the bytes left; `parse` of `to_ie` is the identity |
| `build::{beacon, probe_response, probe_request, auth, assoc_request, assoc_response, deauth, disassoc, action}` and `IeBuilder` | The frames a station sends and the ones the simulator's AP sends |
| `Bss::from_frame(&Mgmt, rx_channel, rssi, now_ms)` | BSSID, SSID, channel (the chip's, else DS), RSSI, capability, `Security::{Open, Wep, Wpa1, Rsn(Rsn), InvalidRsn}`, the whole RSN element for the handshake, country, HT/VHT/HE flags. `None` for IBSS, a spoofed transmitter or a group BSSID |
| `ScanTable::{new(capacity), update, expire(now, max_age), get, by_signal}` | By BSSID. Never past its capacity: when full, a new BSS replaces the weakest only if it is stronger. A hidden beacon never blanks a name learned from a probe response |

The scan time is the caller's millisecond clock; the library has no clock.

### `libs/eapol`

EAPOL-Key frames (`KeyFrame::{parse, encode, mic_input}`), the key-data walker
(`keydata::parse`: RSN element, GTK KDE, `dd 00` padding) and the supplicant:

```rust
let config = Config { akm, pairwise, group, pmk, aa, spa, ap_rsn_ie, assoc_rsn_ie };
let mut sup = Supplicant::new(config, Standard::new(|| csprng_32_bytes()))?;
let actions = sup.input(eapol_pdu)?;   // Vec<Action>, or Err with the state unchanged
```

`Action` is `Send(pdu)`, `InstallPtk { cipher, tk }`, `InstallGtk { index, tx,
key, rsc }` or `Authorized`. Crypto is the `Crypto` trait (`random_nonce`,
`derive_ptk`, `mic`, `key_unwrap`); `Standard` implements it over the WP0
functions of `lazyos-crypto`, tests pin the nonce, and the fuzz target forges
the MIC. AKM 2 (PSK) and 6 (PSK-SHA256) with CCMP-128 are supported; TKIP,
WEP, GCMP, any other AKM and management frame protection are refused by name
(`Error::UnsupportedCipher(Cipher::Tkip)`, `UnsupportedAkm`, `PmfRequired`).

What the supplicant checks, in order, for message 3: stage, **MIC in constant
time before anything in the frame is believed**, replay counter strictly above
every MIC-verified frame (and above message 1's), ANonce equal to message 1's,
Key Length 16, AES key unwrap, key-data walk with bounds and padding rules,
**the RSN element byte for byte equal to the beacon's** (a downgrade is
`RsnMismatch`, deauthentication reason 17), a GTK KDE of 16 octets. Every
`Err` leaves `Supplicant::state()` exactly as it was (a test and the fuzz
target compare it). `Error::is_fatal()` separates "drop the frame" (wrong MIC,
replay, out of order) from "deauthenticate" (an authenticated frame that is
wrong); `Error::deauth_reason()` gives the code.

Message 1 is unauthenticated, so the supplicant keeps up to four candidates
`(ANonce, SNonce, PTK, replay counter)`. Message 3 selects the candidate by
its ANonce and is MIC-verified with that candidate's PTK; the others are
dropped only after a verified message 3. A forged message 1 cannot break a
genuine exchange, and a forged message 3 fails its MIC. Remaining limits: a
flood of more than four message 1s evicts the oldest candidates (the genuine
one included), so the exchange fails and the caller's timer and the AP's retry
restart it, and each forged frame costs a PTK derivation and a message 2.

A retransmitted message 3 gets message 4 again and installs nothing (the
KRACK rule), a group message delivering the installed GTK is answered but not
reinstalled, a message 1 repeating the installed ANonce is refused, and a new
ANonce starts a PTK rekey while the installed key keeps working until message
3 verifies.

### What the caller (`wlanmd`) must do

- **Frames.** Hand `input` the EAPOL PDU (from the EAPOL version octet; the
  Ethernet header and ethertype 0x888E are the caller's) of frames sourced
  from the BSSID. Send `Action::Send` PDUs to the BSSID as EAPOL.
- **Timers** (the library has none): a deadline for the whole 4-way handshake
  after association (about 1 s; the authenticator retries message 1 and 3 at
  100 ms intervals), and for the group handshake; on expiry deauthenticate
  with reason 15. The supplicant never retransmits by itself; it answers what
  the AP retransmits.
- **Key installation order.** Run one `input` call's actions in order. Message
  4 goes out unencrypted, so wait for its transmit status before
  `InstallPtk`, then `InstallGtk`, and open the data port on `Authorized`. A
  group rekey is `Send` then `InstallGtk`. Pass `rsc` to the chip as the
  group key's initial receive counter. Never install a key the library did not
  ask for again.
- **Which RSN element.** `ap_rsn_ie` is the `Bss::rsn_ie` of the BSS being
  joined (whole element); `assoc_rsn_ie` is the element sent in the
  association request and must state exactly one pairwise cipher and AKM.
- **Errors.** `Config` errors (`Supplicant::new`) mean this BSS cannot be
  joined: pick another or report why.
- **The PMK** comes from `keyd` (`WifiPmk`); the supplicant holds it, the
  derived KCK/KEK and the group key in `Key` wrappers that print as `Key<N>(..)`
  and are wiped on drop.

### Evidence

`cargo test -p ieee80211 -p eapol` (frame and element unit tests, RSN round
trips, a test authenticator built from `raw_frame` and `lazyos_crypto` alone,
every refusal checked for no state change); the Python transcript
`tools/wifi/make_eapol_vectors.py` (`--check` in CI; `hashlib`, `hmac`,
`cryptography` only) is the other side of a byte-for-byte comparison of
messages 2, 4 and group 2 for both AKMs; a handshake recorded by someone else (Wireshark's `wpa-Induction.pcap`, fixture
`libs/eapol/src/tests/capture.rs`, extracted by
`tools/wifi/extract_capture_vectors.py`) has its PMK, PTK, three MICs and message 3
key data verified through the library (its TKIP group cipher is refused by the
supplicant, so replies are not compared byte for byte); seeded soaks run with
`FUZZ_CASES=20000 cargo test -p ieee80211 -p eapol --release seeded`; the
cargo-fuzz targets are `fuzz/fuzz_targets/ieee80211.rs` and `eapol.rs`, with
seeds from `fuzz/seeds_wifi.py`.

## WP4: the station manager

```
 wifictl, Network app ──wifi.v1──▶ wlanmd (_wlan 912, no caps) ──WifiPmk──▶ keyd
                                     │  wlansta::Station per radio   ▲ StoreSecretFor
                                     │  profiles ─────────────▶ confd
                                     ▼ wifi.hw.v1 (+ event channel)
                     os.lazy.net.wifi.hw/wlan0: wifid (_wifi 911) or wifisim (_wifisim 913)
```

### Names and identities

| Name | Held by | Enforced by |
|---|---|---|
| `os.lazy.net.wifi.hw/<ifname>` (`hw.v1`, one per radio; `<ifname>` is its `nic.v1` card's name, `wlan0`) | `_wifi`, `_wifisim` (unlabelled, no session) | the registry (`netpolicy::may_register_wifi_hw_name`, `kernel/src/ipc/policy.rs`) |
| `os.lazy.net.wifi` (`wifi.v1`) | `_wlan` | the registry (`netpolicy::may_register_wifi_service_name`) |
| `Attach` to a radio | `_wlan` | the driver (`netpolicy::may_attach_wifi_hw`) |
| `system/net/<if>/wifi/{state,scan}` | `_wlan` | `messengerd` (`netpolicy::is_wifi_topic`) |
| `sys/net/wifi/networks/**`, `user/<uid>/net/wifi/networks/**` | `_wlan` (plus `sys/**` writers and each user for their own) | `confd` (`confd::Caller::wifi`, `netpolicy::is_wlanmd`) |
| `WifiPmk`, `StoreSecretFor`, `DeleteSecretFor` | `_wlan` | `keyd` (`secretstore::authorize`) |

`wlanmd` finds radios by listing the registry every second (as `netd` finds
cards) and checks each owner again; a radio missing for 3 s, or whose event
channel reports its peer gone, is dropped and published `Disabled`, and
attached again when its driver registers anew. At most four radios.

### `libs/wlansta`

The station is pure (`no_std` + `alloc`, no clock, no I/O):

```rust
let mut sta = Station::new(mac, fresh_nonce);           // fn() -> [u8; 32]
let actions = sta.set_profiles(now_ms, profiles);       // auto-connect may start
let actions = sta.input(now_ms, Event::ScanDone { scan_id, aborted, entries });
let actions = sta.pmk(now_ms, &id, Ok(Key::new(pmk)));  // answer to Action::FetchPmk
sta.connect(now_ms, &id)?; sta.disconnect(now_ms); sta.scan(now_ms, ssids)?;
sta.status(); sta.networks(Viewer::User(uid)); sta.next_deadline();
```

`Event`: `ScanStarted`, `ScanDone`, `Rx { kind, rssi_dbm, frame }`,
`BeaconLoss`, `Deauthenticated`, `HwError { op, errno }`, `Tick`.
`Action`: `Scan`, `AbortScan`, `Join`, `Tx`, `SetState`, `SetKey`, `Leave`,
`FetchPmk`. The caller runs a batch in order and stops at the first radio
call that fails, reporting it as `HwError`. Also: `policy` (who may do what on
`wifi.v1`), `profile` (the `confd` keys, ids `w` + 12 hex digits, the country
default `00`), `ssid::display` (lossy UTF-8, control and invisible
formatting characters dropped, at most 32 octets), `select` (security
classification and BSS choice).

Behaviour, each pinned by a host test:

- **Selection**: a known SSID whose security is the profile's; priority, then
  signal. Joinable: Open, and RSN with CCMP-128 pairwise and group, AKM 2
  (preferred) or 6, no MFP required. Anything else is `Unsupported`, listed
  and never joined (an explicit `Connect` to it fails `PolicyMismatch`).
  A BSS heard in the last 30 s is joined without scanning; hidden profiles are
  probed for by name.
- **Join**: `Join`, Open System authentication, `SetState(Authenticated)`,
  an association request (the BSS's rates; for WPA2 an RSN element with
  exactly the chosen AKM and CCMP, which `eapol` checks message 2 and 3
  against), `SetState(Associated, aid)`; open networks then
  `SetState(Authorized)`. Requests are sent 4 times, 500 ms apart.
- **Handshake**: EAPOL from the BSSID goes to `eapol::Supplicant`; its
  actions become `TxMgmt(Eapol)`, `SetKey(Pairwise, 0)`, `SetKey(Group, n,
  rsc)` and `SetState(Authorized)` in its order. 5 s deadline from
  association. Message 2 sent and no verified message 3 (deadline, or the AP
  deauthenticating) is `BadPassphrase`, which blocks that profile until the
  user connects to it again: no retry storm.
- **Connected**: group and PTK rekeys keep flowing; no timer runs (a forged
  message 1 cannot make the station drop its own link).
- **Failures** (`Rejected`, `Timeout`, `Deauthenticated`, `BeaconLoss`,
  `PolicyMismatch`, `Radio`, `NotFound` for an explicit connect) leave the BSS
  and retry after 1 s, doubling to 60 s. Auto-connect that finds nothing goes
  `Idle` and scans again every 60 s. `Disconnect` suspends auto-connect until
  the next `Connect`.
- **Hostile input**: frames not from the joined BSS, not to us, malformed,
  protected or out of place are dropped and counted without any state change;
  scan entries with an unknown band or channel are dropped.

Tests: `cargo test -p wlansta` (joins with both AKMs against a test AP whose
authenticator is built from `lazyos-crypto` alone, group rekey, wrong
passphrase, deauth, beacon loss, retransmissions and back-off, scan timeout,
forged EAPOL, policy and profile rules) and a seeded fuzz over radio events
and commands with invariants after every step
(`FUZZ_CASES=20000 cargo test -p wlansta --release seeded`).

### `wlanmd` and `wifictl`

`wlanmd` (`user/src/bin/wlanmd.rs`, `wlanmd/{radio,service,store}.rs`) runs
as `_wlan` with no capabilities in `LAZYOS_WIFI=1` images (which need
`LAZYOS_NETD=1`; `init` row after `confd` and `keyd`). It reads the profiles
from `confd`, passes `sys/net/wifi/country` (default `00`) to each radio with
`SetCountry`, auto-connects to the best known system network, and serves
`wifi.v1` with the rules of `wlansta::policy`: `AddNetwork` scope `System`
only from `elevd`, a user network only for its owner, the passphrase straight
to `keyd` (`StoreSecretFor`) and never kept or logged. User profiles join
auto-connect once logins are tracked (`Station::set_active_users`, WP5);
today they are joined by an explicit `Connect`. Serial lines:
`WLANMD:UP radios=<n>`, `WLANMD:RADIO:UP|GONE|FAIL`, `WLANMD:STATE <if>
<state> reason=<reason> ssid="<ssid>"`, `WLANMD:SCAN <if> gen=<n>
networks=<n>`, `WLANMD:ADD`, `WLANMD:FORGET`.

`wifictl` (native, on the kernel's list): `status`, `scan`, `list`,
`connect SSID` (asks for a new WPA2 network's passphrase without echo, or
reads `WIFICTL_PASSPHRASE`; `AddNetwork` in the user scope, then `Connect`
and wait for `Connected` or `Failed`), `disconnect`, `forget ID`.

### What a radio (`wifid`, `wifisim`) must do for `wlanmd`

- Register `os.lazy.net.wifi.hw/<ifname>` and `os.lazy.net.nic/<ifname>` with
  the same `<ifname>`; refuse `Attach` from anyone but `_wlan`
  (`netpolicy::may_attach_wifi_hw`, `EPERM`) and owner-only calls from
  anyone but the attached task.
- `ScanResults` entries and `RxMgmt(Mgmt)` frames start at the 802.11 MAC
  header (no FCS); `RxMgmt(Eapol)` and `TxMgmt(Eapol)` are 802.3 frames
  (destination, source, 0x888E, PDU). `ScanDone` once per scan, after its
  results are readable.
- `TxMgmt` frames go out under the keys in force when queued: message 4 is
  queued just before `SetKey(Pairwise)` and must leave in the clear.
- `SetState(Authorized)` straight after `Associated` for an open network.
- Deliver deauthentication and disassociation from the BSS as
  `Deauthenticated` (and/or the frame as `RxMgmt`; `wlanmd` acts on the first
  and ignores the repeat).
- The NIC's link (`system/net/<if>/link`, published by the driver) follows
  `Authorized`; `messengerd` lets only `_net` publish link topics today, so the
  Wi-Fi driver uids need adding there.
