# Wi-Fi

The plan is [wifi-prerequisites-plan.md](../wifi-prerequisites-plan.md) (the
stack above the driver) and [wifi-plan.md](../wifi-plan.md) (the hardware and
licence). This page describes what is built, stage by stage. Only WP3 is here
so far.

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
messages 2, 4 and group 2 for both AKMs; seeded soaks run with
`FUZZ_CASES=20000 cargo test -p ieee80211 -p eapol --release seeded`; the
cargo-fuzz targets are `fuzz/fuzz_targets/ieee80211.rs` and `eapol.rs`, with
seeds from `fuzz/seeds_wifi.py`.
