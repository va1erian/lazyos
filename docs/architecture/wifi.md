# Wi-Fi

The plan is [wifi-prerequisites-plan.md](../wifi-prerequisites-plan.md) (the
stack above the driver) and [wifi-plan.md](../wifi-plan.md) (the hardware and
licence), with the RTL8822CE driver plan in
[wifi-rtw88-plan.md](../wifi-rtw88-plan.md). This page describes what is
built: the protocol libraries (WP3), WP4's station half (`wlansta`,
`wlanmd`, `wifictl`) and its radio side (the soft-MAC and the simulated
radio).

## WP3: the 802.11 protocol libraries

Two host-tested, fuzzed, `no_std` + `alloc` libraries with no `unsafe`, used by
the station manager `wlanmd` (WP4) and cross-checked by the simulator's access
point (`tools/wifi/fake_ap.py`, WP4). As first written they parsed only
management frames and EAPOL, on the assumption that the chip converts the data
path; the RTL8822CE does not, so `ieee80211` gained data frames and CCMP
framing for the soft-MAC (WP4, below). Both are written from IEEE Std
802.11-2020 alone (`THIRD_PARTY.md`).

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
| `Elements::parse(ies)` | SSID (may be empty, all NULs, non-UTF-8; `hidden()`), rates, DS channel, country, HT/VHT/HE capabilities as raw slices, the HT Operation element (22 octets or more; `ht_primary_channel()`), the first RSN element (body and whole element), the WPA1 vendor element, whether a WMM element was present (`wmm`, noted past the vendor cap too), up to 32 vendor elements |
| `Rsn::{parse_body, parse_ie, to_body, to_ie}` | Version, group cipher, pairwise list, AKM list, capabilities (`RsnCaps`), PMKID list, group management cipher. Optional tail fields default per the standard; counts are checked against the bytes left; `parse` of `to_ie` is the identity |
| `build::{beacon, probe_response, probe_request, auth, assoc_request, assoc_response, deauth, disassoc, action}` and `IeBuilder` | The frames a station sends and the ones the simulator's AP sends |
| `Bss::from_frame(&Mgmt, rx_channel, rssi, now_ms)` | BSSID, SSID, channel (`operating_channel`: the DS Parameter Set's on 2.4 GHz, where a receiver also hears the overlapping neighbour channels, the HT Operation primary channel on 5 GHz, a claim from the other band ignored, else the channel heard on), RSSI, capability, `Security::{Open, Wep, Wpa1, Rsn(Rsn), InvalidRsn}`, the whole RSN element for the handshake, country, HT/VHT/HE/WMM flags and the HT primary channel. `None` for IBSS, a spoofed transmitter or a group BSSID |
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
let actions = sta.set_active_users(now_ms, uids);       // their profiles too
let actions = sta.input(now_ms, Event::ScanDone { scan_id, aborted, entries });
let actions = sta.pmk(now_ms, owner, &id, Ok(Key::new(pmk))); // answer to FetchPmk
sta.connect(now_ms, owner, &id)?; sta.disconnect(now_ms); sta.scan(now_ms, ssids)?;
sta.status(); sta.networks(Viewer::User(uid)); sta.next_deadline();
sta.current_owner(); sta.heard_ssid(typed);
```

`Event`: `ScanStarted`, `ScanDone`, `Rx { kind, rssi_dbm, frame }`,
`BeaconLoss`, `Deauthenticated`, `HwError { op, errno }`, `Tick`.
`Action`: `Scan`, `AbortScan`, `Join`, `Tx`, `SetState`, `SetKey`, `Leave`,
`FetchPmk`. The caller runs a batch in order and stops at the first radio
call that fails (a failed `Leave` or `AbortScan` excepted), reporting it as
`HwError`. Also: `policy` (who may do what on `wifi.v1`), `profile` (the
`confd` keys, ids `w` + 12 hex digits, the country default `00`,
`normalize`), `ssid::display` (lossy UTF-8; control characters, bidi marks,
embeddings, overrides and isolates, line and paragraph separators,
zero-width space, tag and annotation characters dropped, the zero-width
joiner and non-joiner kept; at most 32 octets) and `ssid::matches`,
`select` (security classification and BSS choice).

**Identity.** A profile is its owner and its id (`Profile::is`): each user
writes their own `confd` subtree, so two users may hold the same id.
`profile::normalize` keeps system profiles first, drops a user profile that
reuses a system id (or repeats its own), and keeps at most 32 per owner,
counted after parsing (`wlanmd` reads at most 64 ids per owner).
`Connect`, `Forget`, the PMK and the station's target, blocks and status
all carry the owner.

Behaviour, each pinned by a host test:

- **Selection**: a known SSID whose security is the profile's; a BSS that
  failed us lately last, then priority, then signal. Joinable: Open, and RSN
  with CCMP-128 pairwise and group, AKM 2 (preferred) or 6, no MFP required.
  Anything else is `Unsupported`, listed and never joined (an explicit
  `Connect` fails `Unsupported`; one whose security differs from the
  profile's, `PolicyMismatch`). A BSS heard in the last 30 s is joined
  without scanning unless it failed us lately; hidden profiles are probed
  for by name. A user's profiles are auto-connect candidates only while that
  user is logged in (`set_active_users`); their network is left when they
  log out.
- **Join**: `Join` on the BSS's own channel, Open System authentication,
  `SetState(Authenticated)`, an association request (the BSS's rates without
  membership selectors; for WPA2 an RSN element with exactly the chosen AKM
  and CCMP, which `eapol` checks message 2 and 3 against; for an HT BSS an
  HT Capabilities element: 20 MHz, SM power save disabled, MCS 0-7, no
  aggregation; for a WMM BSS a WMM Information element),
  `SetState(Associated, aid)`; open networks then `SetState(Authorized)`.
  Requests are sent 4 times, 500 ms apart. The PMK must come within 5 s.
- **Handshake**: EAPOL from the BSSID goes to `eapol::Supplicant`; its
  actions become `TxMgmt(Eapol)`, `SetKey(Pairwise, 0)`, `SetKey(Group, n,
  rsc)` and `SetState(Authorized)` in its order. 8 s deadline from
  association (an AP gives up after about four and deauthenticates with
  reason 15). After message 2 a deauthentication with reason 2, 15 or 23 is
  `BadPassphrase`; any other end (the deadline, another reason: a lost
  frame, a forged deauthentication and a rogue BSS look alike) is `Timeout`
  or `Rejected` and a strike, and the second strike in a row is
  `BadPassphrase`. `BadPassphrase` blocks auto-connect to that profile for
  15 minutes, or until the user connects to it again: no retry storm, and no
  permanent lockout from one forged frame.
- **Connected**: group and PTK rekeys keep flowing; no timer runs (a forged
  message 1 cannot make the station drop its own link).
- **Failures** (`Rejected`, `Timeout`, `Deauthenticated`, `BeaconLoss`,
  `PolicyMismatch`, `Unsupported`, `Radio`, `NotFound` for an explicit
  connect) leave the BSS and retry after 1 s, doubling to 60 s; the back-off
  resets only once a link has held 30 s. `Rejected`, `Timeout` and
  `BeaconLoss` put the BSSID on a 60 s penalty: another BSS of the SSID is
  tried, or a scan first. Auto-connect that finds nothing goes `Idle` and
  scans again every 60 s. A join scan that times out joins only what was
  heard in the last 30 s. `Disconnect` suspends auto-connect until the next
  `Connect`.
- **Hostile input**: frames not from the joined BSS, not to us, malformed,
  protected or out of place are dropped and counted without any state change;
  scan entries with an unknown band or channel are dropped.

Tests: `cargo test -p wlansta` (joins with both AKMs against a test AP whose
authenticator is built from `lazyos-crypto` alone, group rekey, wrong
passphrase evidence and block decay, deauth, beacon loss, retransmissions,
penalties and back-off, scan timeout, forged EAPOL, the association
request's elements, identity across owners, logged-in users, policy and
profile rules) and a seeded fuzz over radio events and commands with
invariants after every step
(`FUZZ_CASES=20000 cargo test -p wlansta --release seeded`).

### `wlanmd` and `wifictl`

`wlanmd` (`user/src/bin/wlanmd.rs`, `wlanmd/{radio,service,sessions,store}.rs`)
runs as `_wlan` with no capabilities in `LAZYOS_WIFI=1` images (which need
`LAZYOS_NETD=1`; `init` row after `confd` and `keyd`). It reads the profiles
from `confd` (again every 10 s; a read that fails keeps the profiles known,
`WLANMD:PROFILES:STALE`), passes `sys/net/wifi/country` (default `00`; a code
that is not an assigned ISO 3166-1 code by `wifimac::regdom` is `00` too) to
each radio with `SetCountry` (the world domain when the radio refuses the
code, no attach when it refuses that too; a changed key is applied within
10 s), auto-connects to the best known system network and to the networks of
the users logged in, and serves `wifi.v1` with the rules of
`wlansta::policy`: `AddNetwork` scope `System` only from `elevd` and
without a passphrase (the profile under the id whose secret `elevd` stored
in `keyd` through `net.wifi.system`; `keyd` refuses `wlanmd` a `system`
secret), a user network only for its owner, its passphrase straight to `keyd`
(`StoreSecretFor`) and never kept or logged (the request's receive buffer
and body are wiped), a request's id resolved among the profiles the caller
may use, `Disconnect` of a user's network only by its owner, `Scan` once a
second per caller (`EAGAIN`), and `Status` and the retained state topic
showing a user network's profile id only to its owner. `keyd` refusing a PMK
(`EPERM`, `EINVAL`) is retried and printed (`WLANMD:PMK:REFUSED`), never
taken for a wrong passphrase.

Who is logged in comes from `logind`: its login events on `init`'s broker
are only hints (the broker takes any publisher), on which `wlanmd` asks
`logind`'s `Sessions` and believes the answer (and every 30 s besides; an
unanswered question after an `end` hint drops that user, never adds one).
Serial lines: `WLANMD:UP radios=<n>`, `WLANMD:RADIO:UP|GONE|FAIL`,
`WLANMD:STATE <if> <state> reason=<reason> ssid="<ssid>"`, `WLANMD:SCAN <if>
gen=<n> networks=<n>`, `WLANMD:USERS <uids>`, `WLANMD:ADD`, `WLANMD:FORGET`.

`wifictl` (native, on the kernel's list): `status`, `scan`, `list`,
`connect SSID` (asks for a new WPA2 network's passphrase without echo, or
reads `WIFICTL_PASSPHRASE`; 8 to 63 octets of UTF-8 or 64 hex digits;
`AddNetwork` in the user scope, then `Connect` and wait for `Connected` or
`Failed`), `connect --id ID` (a remembered network, when two show alike),
`connect --hidden [--open] SSID`, `disconnect`, `forget ID`. `wlanmd`
matches a typed name against the raw SSIDs it heard first, then against
their displayed form (refusing a guess between look-alikes).

### What a radio (`wifid`, `wifisim`) must do for `wlanmd`

- Register `os.lazy.net.wifi.hw/<ifname>` and `os.lazy.net.nic/<ifname>` with
  the same `<ifname>`; refuse `Attach` from anyone but `_wlan`
  (`netpolicy::may_attach_wifi_hw`, `EPERM`) and owner-only calls from
  anyone but the attached task with the credentials it attached with.
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
  `Authorized`; `messengerd` lets `_net`, `_wifi` and `_wifisim` publish
  link topics.

## WP4, radio side: the soft-MAC and the simulated radio

The RTL8822CE converts nothing between 802.3 and 802.11 and leaves the CCMP
packet number to software, so the radio programs do in software what the old
plan expected of firmware. Both radio programs (`wifid` over `libs/rtw88`,
`wifisim` over a simulated air) share one soft-MAC and differ only in their
`Radio`:

```
 netd   ◄── nic.v1  os.lazy.net.nic/wlan0 (kind wireless) ──┐
 wlanmd ◄── wifi.hw.v1  os.lazy.net.wifi.hw/wlan0 ──────────┤  user::wifi (server, hw, nic)
                                                            │  libs/wifimac SoftMac
                                                       Radio trait
                         wifid: libs/rtw88 ────────────┴──────── wifisim: libs/simair over air0
```

### `libs/ieee80211` additions

| Item | What it does |
|---|---|
| `data::DataHeader::parse` | The MAC header of a data frame: three or four addresses, QoS Control, the HT Control length, To-DS/From-DS, BSSID, source and destination |
| `data::encap(eth, &Encap)` | Ethernet II to a To-DS (station) or From-DS (AP) data frame, QoS with a TID or plain, an optional security header, RFC 1042 or 802.1H bridge tunnel (AARP, IPX) |
| `data::decap(header, body, out)` | One MSDU or every A-MSDU subframe to Ethernet II; refuses a non-SNAP body, fragments, a malformed A-MSDU and the injected first subframe of CVE-2020-24588; a refused A-MSDU delivers nothing |
| `data::{user_priority, is_eapol, amsdu_body}` | The TID from a VLAN tag, IPv4 TOS or IPv6 traffic class (EAPOL is 7); EAPOL by EtherType 0x888E; an A-MSDU body (tests, simulator) |
| `ccmp::{header, parse_header, framing, Replay, rsc_from_bytes}` | The 8-octet CCMP header and its 48-bit PN; the CCM nonce and AAD of a data or management frame (12.5.3.3, the Annex J.6.4 vector); a replay window per TID plus one for non-QoS, seeded with the key's RSC |
| `class::{classify, classify_msdu}` | Management (subtype, protected), data, null data, control or ignore; EAPOL or other for a decapsulated frame |

The fuzz entry decapsulates and re-encapsulates every data frame it can parse
(the Ethernet frames must come back equal), builds its CCMP framing and runs
a replay-window script; `fuzz/seeds_wifi.py` has data, A-MSDU, injected
A-MSDU, CCMP, four-address and null seeds.

### `libs/wifimac`: the `Radio` trait and the `SoftMac`

```rust
pub trait Radio {
    fn info(&self) -> RadioInfo;                 // mac, bands, ciphers, max_frame, key_slots
    fn set_address(&mut self, mac: Mac) -> Result<(), RadioError>;
    fn tune(&mut self, channel: Channel) -> Result<(), RadioError>;
    fn set_bssid(&mut self, bssid: Option<Mac>, aid: u16) -> Result<(), RadioError>;
    fn set_scan_filter(&mut self, scanning: bool) -> Result<(), RadioError>;
    fn set_state(&mut self, state: StaState) -> Result<(), RadioError>;
    fn set_key(&mut self, key: &KeyInstall<'_>) -> Result<(), RadioError>;
    fn del_key(&mut self, kind: KeyKind, index: u8) -> Result<(), RadioError>;
    fn transmit(&mut self, frame: &[u8], options: &TxOptions) -> Result<(), RadioError>;
    fn tx_ready(&self) -> bool;
    fn receive(&mut self) -> Option<Received>;
}
```

Frames cross it from the MAC header, without FCS. **Transmit:** a frame with
`TxOptions::protected` already carries its CCMP header (the soft-MAC picked
the PN) and the plaintext after it; the radio encrypts that body with the key
the key id and the receiver address name and appends the MIC (on the
RTL8822CE: hardware encryption, software IV). `basic_rate` marks management,
EAPOL and probes; `priority` is the TID for the access-category queue.
**Receive:** the radio decrypts with its keys and hands the frame over with
the MAC header and the CCMP header kept (`Received::decrypted`) and any MIC
it kept counted in `Received::trailer`, so the soft-MAC checks the PN for
replays and strips the trailer; a frame it could not decrypt it drops (or
passes undecrypted, which the soft-MAC drops). The radio never parses a body:
it filters by address (`set_bssid`, `set_scan_filter`) and picks a key by key
id and receiver.

`SoftMac<R: Radio>` is `hw.v1` as pure logic with the clock injected:

* **Owner.** `attach(caller)` (`Busy` when owned) remembers a `Caller`:
  the task slot and its credentials (uid, label, session), all compared on
  every later call, so a slot reused by another process before the owner's
  exit is noticed owns nothing; every other call from anyone else is
  `Denied`; `detach` and `release` (the owner exited or its channel broke)
  stop the scan, deauthenticate, delete every key and forget the country
  and the queued events.
* **Country.** Nothing transmits before `SetCountry` (`Again` from an active
  scan, `Join`, `TxMgmt`); a passive scan runs in the world domain. A code
  must be `00` or an assigned ISO 3166-1 alpha-2 code (`iso3166`, 249
  codes; `ZZ` is `Invalid`). The domain table (`regdom`) is coarse: the
  world domain probes only on 1-11, the FCC group stops at 11, other
  countries get 12-13; UNII-1 and UNII-3 are active for a named country, DFS
  channels always passive, no 6 GHz. A new country re-plans a running scan
  (forbidden channels skipped, probes only where now allowed) and leaves a
  joined BSS whose channel it forbids, silently, reporting
  `Deauthenticated` (reason 3).
* **Scan.** Software stepping: tune, one probe request per SSID (or a
  wildcard) where the domain allows probing, dwell 50 ms active / 120 ms
  passive (or the request's 10-1000 ms), next. Beacons and probe responses
  heard meanwhile are kept raw, the latest per BSSID and kind, each at most
  `max_mgmt` octets, at most 256 and 256 KiB; a full table drops its
  weakest entry for a new one at least as strong, and a copy heard on
  another (adjacent) channel replaces an entry only when stronger.
  `ScanResults` pages them with their age (64 entries and 12 KiB of frames
  per page). A scan while joined returns to the BSS's channel and pauses the
  beacon watch.
* **Where it transmits.** The soft-MAC tracks the channel the radio is
  tuned to. Nothing goes to the BSS while a scan step has the radio
  elsewhere, nor, on a channel the domain lists as passive, before the BSS
  was heard there (a fresh scan entry or a beacon since the join): `TxMgmt`
  frames are built at once (sequence, PN, the keys in force) and held, at
  most 16, until the radio may send them; the network stack's frames stay in
  its ring (`tx_ready` is false). A deauthentication on `Leave` retunes to
  the BSS first. Probe requests go out only on scan steps where the domain
  allows probing.
* **Join** tunes and filters to the BSS and learns from its scan entry (then
  its beacons) the beacon interval and whether it speaks QoS (HT or WMM):
  data goes as QoS data with the TID from the user priority, or plain data.
* **TxMgmt.** A management frame must be a station subtype (association,
  reassociation, probe, disassociation, authentication, deauthentication,
  action) with no flags and our own transmitter address; the soft-MAC sets
  its sequence number. Its receiver and BSSID must be the BSS's (a probe
  request may be broadcast in either). EAPOL (802.3, from our address to
  the BSSID) goes as a data frame to the BSS, in the clear before the
  pairwise key and protected after it; one too long for the radio once
  wrapped is refused before it takes a PN.
* **SetState** drives the network card's link: up only in `Authorized`,
  which comes only from `Associated` and needs the pairwise key once any key
  exists or the BSS advertised privacy or RSN (in the scan entry the join
  learned from, or any beacon since; sticky for the join). Below
  `Authorized` the receive queue is emptied; below `Associated` the keys are
  deleted.
* **Keys.** CCMP-128 only: pairwise id 0-1, group id 1-3, for the joined
  BSSID; IGTKs are refused (no management frame protection). The TX PN
  belongs to the association: it starts at 1, is never wrapped, and carries
  on across a rekey or a key deleted and installed again (only leaving the
  BSS, a deauthentication or a state below `Associated` starts it over), so
  a temporal key installed twice never repeats a PN. Each key has its replay
  window. A key the radio refuses leaves its slot empty in both the radio
  and the table.
* **Events** (at most 64 queued, `RxMgmt` at most 48 of them): `ScanDone`;
  `RxMgmt` (from the joined BSS to us, and only when the owner can be
  waiting for it: an authentication or (re)association response after its
  request was sent, action frames once associated, EAPOL as 802.3 once the
  AP answered the association, deauthentication and disassociation always;
  a retransmission of the last unprotected unicast frame is dropped; at
  most 32 a second); `BeaconLoss` (no beacon from the BSS on its channel for
  ten intervals, at least a second, the interval capped at 1000 ms, once per
  silence); `Deauthenticated` (which also closes the port and deletes the
  keys; never rate-limited). A control event that finds the queue full
  evicts the oldest `RxMgmt`; a repeat of an unread `BeaconLoss` or
  `Deauthenticated` folds into it; drops count in `mgmt_dropped`. When the
  owner's inbox is full, the event goes back to the front of the queue.
* **Data path.** Received data must come From-DS from the BSSID to us or a
  group; a protected frame needs `decrypted` and a fresh PN in its key's
  window; plaintext is refused once the BSS is protected; our own reflected
  group frames are dropped; EAPOL goes to the owner, never to `netd`; the rest
  waits (at most 256 frames) for `netd` only while authorized. Transmit
  refuses EAPOL and foreign source addresses from `netd`. The MTU is what
  fits a QoS frame with a CCMP header in the radio's largest frame (1442 on
  the simulator's air).

### `user::wifi`: serving it

`user::wifi::run(radio, ifname, own_uid)` registers
`os.lazy.net.wifi.hw/<ifname>` and `os.lazy.net.nic/<ifname>` and serves both
from one loop that also waits on the radio's wake-up endpoint
(`Hardware::wake_endpoint`) and pumps it (`Hardware::pump`). Nothing a client
sends ends the loop: a request too large for the 16 KiB receive buffer (the
kernel dequeues it, then answers `E2BIG`) or one that does not decode is
dropped and counted (`WIFI:DROPPED`), a reply that cannot be delivered is
the client's loss; only the service endpoints going away is fatal. The loop
polls again at once when the radio's receive budget ran out. The card side
is `netdrv`'s engine (`libs/nicdrv`) over `MacRings`, the soft-MAC's data
path as the engine's card, so `netd` sees the same rings and the same
hostile-client rules. The owner's events go on the channel it passed in
`Attach`; a dead channel (or one the once-a-second probe finds dead, which
also drains whatever the owner sent there) releases the radio. Who may
attach: an unlabelled system service outside any session running as
`_wlan` (912), and in `wifisim` alone `_wifisim` itself the same way (the
`wifiprobe` it spawns); root is not named. Who may drive the card
(`AttachRing`, `SetRxMode`, `DetachRing`, `Kick`; the kernel does not apply
`netpolicy::NIC_CLIENT_RULES`, so `nic::may_drive` does): `_netd`
unlabelled outside any session, root unlabelled (console diagnostics), and
`_wifisim` for an `air*` card only; `Info` and `Stats` are open. The
temporal key of a `SetKey` is wiped from the decoded arguments, the parcel
and the receive buffer once handled. `netd` sizes an interface's frames by
the card's MTU.

### `wifisim` and the simulated air

`wifisim` (uid `_wifisim` 913, no capability; `LAZYOS_WIFI_SIM=1`, which
needs `LAZYOS_NETD=1`) is the simulator's `Radio`: 802.11 frames inside
Ethernet frames on a second virtio-net card, with software CCMP (`libs/simair`
over RustCrypto `ccm` through `lazyos_crypto::wifi::ccm`; its test is the
whole Annex J.6.4 MPDU). The wire format (`libs/simair`,
`tools/wifi/fakeap/air.py`):

```
Ethernet  dst ff:ff:ff:ff:ff:ff  src <card>  type 0x88B5 (IEEE Local Experimental 1)
air       version 1 | band (0 2.4 GHz, 1 5 GHz) | channel | rssi i8 | flags (bit 0 basic rate) | 0 | length u16 BE
802.11 frame (MAC header to MIC, no FCS), at most 1492 octets
```

The radio hears a frame only on the band and channel it is tuned to, to its
address or (group-addressed) from its BSS, and every beacon or probe response
while scanning.

**The air card** must not become a network interface. The harness gives it
the MAC `02:57:53:49:4d:00` (`netpolicy::SIM_AIR_MAC`); in a
`LAZYOS_WIFI_SIM=1` image `netdrv` serves that card as `os.lazy.net.nic/air0`
instead of the `eth` name `devd` counted, without its ARP self-test
(`NETDRV:AIR ifname=air0`), and `netd` never attaches a card whose name starts
with `air` (`netpolicy::stack_attaches`). The name stays in the registry's
driver-only NIC namespace, `netpolicy` lets `_wifisim` be its client, and
`wifisim` checks that a NIC driver identity holds it.

`wifiprobe` (embedded with the simulator; `wifisim probe=<mode>` through
`LAZYOS_WIFI_SIM_ARGS`) is a test client of `hw.v1` for images without
`wlanmd`: `scan`, `open:<ssid>`, `wpa2:<ssid>:<passphrase>` and the harness's
`suite`, which runs the 4-way handshake through `libs/eapol` with a PMK it
derives itself.

### The fake access points and the harness

`tools/wifi/fake_ap.py` is the other end of the air (QEMU
`-netdev socket,udp=...,localaddr=...`): LazyOpen (open, ch 1), LazyWPA2
(WPA2-PSK, ch 6), LazyHidden (hidden, ch 11), LazyRoam (WPA2 on ch 1 and 11)
and LazyWrong (a passphrase no station has). It beacons, answers probes,
authenticates, associates, runs the WPA2 authenticator written from the
standard with `hashlib`, `hmac` and `cryptography` (PRF-384, HMAC-SHA1 MIC,
AES key wrap, AES-CCM; nothing shared with `libs/eapol` or `ieee80211`), and
serves DHCP (192.168.77.0/24), ARP and ICMP behind every BSS, pinging the
station after its lease. Faults on its control port or `--schedule`:
`deauth`, `silence` (beacon loss), `rekey` (group key handshake). Every frame
goes to a pcap (link type 105), every action is an `AP:` line.

```bash
cargo test -p ieee80211 -p wifimac -p simair -p lazyos-crypto -p netpolicy
python tools/wifi/test_fake_ap.py         # the AP against a scripted station
python tools/wifi/test_judge.py           # the judge fails when it should
python tools/wifi/sim_run.py              # build, boot, judge (shots/wifi_sim/)
python tools/run_demo.py --wifi-sim       # interactive: wlan0 against the fake APs
python tools/run_demo.py --wifi-probe open:LazyOpen
```

`sim_run.py` judges from the AP's log, the pcap and the serial log: the air
card is `air0` and `netd` leaves it alone; the scan lists the networks (both
LazyRoam BSSes, the hidden one through a directed probe); the wrong passphrase
is refused at message 2; the open join gives `netd` a DHCP lease on `wlan0`
and the AP's pings are answered; the WPA2 join shows messages 1-4 in the
capture, CCMP data the AP decrypted with its own code, a lease and pings; a
group rekey, beacon loss and a deauthentication each reach the radio's
client.

With `--station wlanmd` the image has no probe: the harness logs in as
`user` and types `wifictl scan`, `wifictl connect LazyWrong` (a wrong
passphrase), `wifictl connect LazyOpen`, `wifictl connect LazyWPA2`
(answering the no-echo prompt when the network is not yet remembered),
then injects a group rekey, beacon loss and a deauthentication.
`sim_judge_wlanmd.py` (self-test `test_judge_wlanmd.py`) wants `wlanmd`
(uid 912) as the radio's owner, `wifictl`'s merged network list, a
`bad-passphrase` failure, both joins with the same AP-side and capture
evidence as the probe run, the link kept across the rekey, and `wlanmd`
joining again on its own after the beacon loss and after the
deauthentication (the AP authorizes it three times). The image needs
BusyBox for the shell (`LAZYOS_BUSYBOX`, or `tools/abi/busybox.py`).

## WP5: front ends

The desktop reaches `wlanmd` only through `os.lazy.net.wifi.v1` and its
retained topics, like `wifictl` does:

| Front end | What it does |
|---|---|
| Network app, Wi-Fi tab (`xui-app/src/bin/network/wifipage.rs`) | Lists `Networks`, joins with `AddNetwork` (scope `User`) + `Connect`, `Disconnect`, `Forget`, `Scan`; refreshes `Status` and `Networks` once a second. The tab exists whenever the image ships `wlanmd` (the program is the test: the window is built once) and adopts the service and a radio that attach later; calls back off after a failure. The passphrase field is a password edit, emptied when Join is pressed |
| Network Status applet (`xui-app/src/bin/netstatus.rs`) | Subscribes to `system/net/+/wifi/state`; signal bars and a tooltip while the primary interface is the joined wireless one |
| Client and model (`xui-app/src/net/wifi.rs`, `wifimodel.rs`) | Bounded calls (1 s), names cleaned and clipped again, host-tested rows, status lines, signal bars and input checks |

Serial lines (`NETAPP:WIFI:*`) never carry a passphrase; `python
tools/wifi/ui_run.py` checks that. Still missing: a system-wide profile
from the UI (`elevd` stores the secret but does not call `AddNetwork`) and the
country in Settings.
