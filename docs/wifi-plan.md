# LazyOS Wi-Fi — exploration and plan (blue sky)

> **Status: exploratory, revision 1 (2026-10-02). Nothing here is built.**
> This answers one question: can LazyOS get Wi-Fi by extracting driver code
> from the Linux kernel (or the BSDs), licence permitting, once networking is
> fully working? The target device is a MediaTek **MT7925** (Wi-Fi 7, M.2
> PCIe); a cheap USB dongle is acceptable if it is easier. The answer is yes,
> with a specific hardware recommendation, a specific licence recommendation
> and a staged plan (W0–W6). Every size and licence below was checked against
> the actual source trees on the date above; the few things that could not be
> checked from here are marked *unverified*.

Related: [networking-plan.md](networking-plan.md) (N0–N5 built, Wi-Fi a §12
non-goal), [architecture/networking.md](architecture/networking.md),
[usb-hid-plan.md](usb-hid-plan.md) and [architecture/usb.md](architecture/usb.md)
(the xHCI driver a dongle would ride on), [driver-plan.md](driver-plan.md)
(claims, DMA, interrupts), [real-pc-boot-plan.md](real-pc-boot-plan.md) (the
M.2 card needs a real PC), [security-model.md](security-model.md) §8 and
[driver-config-plan.md](driver-config-plan.md) §1 (passphrases belong to
`keyd`), [doom-port-plan.md](doom-port-plan.md) (the precedent for
fetch-at-build third-party code and firmware-sized assets).

## 1. Question and short answer

**Question.** Linux has a working MT7925 driver. Can we take it, respect its
licence, and run it on LazyOS, so that a laptop with that card (or a USB
dongle) gets Wi-Fi once `netd` and the socket layer are solid?

**Short answer.**

1. **The MT7925 driver itself is extractable.** It lives in
   `drivers/net/wireless/mediatek/mt76/` and is **permissively licensed**:
   ISC in every kernel up to 6.18, **BSD-3-Clause-Clear** from 6.19 on
   (commit `a96fed2`, "wifi: mt76: relicense to BSD-3-Clause-Clear",
   2025-11-24, at MediaTek's request). Both are GPLv3-compatible. The MT7925
   path contains no GPL-only file. It also has a **mainline USB variant**
   (`mt7925u`, since Linux 6.7; Netgear A8500 `0846:9050` and A9000
   `0846:9072` are MT7925 USB adapters).
2. **What is not extractable is everything the driver sits on**: `mac80211`
   (92.6 kLOC) and `cfg80211` (56 kLOC) are **GPL-2.0-only**, which is
   incompatible with LazyOS's GPL-3.0-or-later. We do not want them anyway:
   they are a general-purpose 802.11 stack for a monolithic kernel. The
   station-mode subset LazyOS needs (scan, authenticate, associate, WPA2
   4-way handshake, install keys, pass Ethernet frames) is a few thousand
   lines, and OpenBSD ships an ISC-licensed one, including the 4-way
   handshake, in `sys/net80211` (24.7 kLOC total; the parts we need are
   ~17 kLOC). FreeBSD has already done the exact exercise of running `mt76`
   over a BSD-licensed 802.11 stack through a ~16 kLOC shim (`linuxkpi_wlan`,
   BSD-2-Clause), and compiles `mt7925` with USB enabled.
3. **The firmware is redistributable** (`LICENCE.mediatek`), as separate
   files that must not become "part of" a GPL work. 1.39 MiB RAM code +
   188 KiB MCU patch per chip, fetched at build time and hash-pinned like
   Freedoom.
4. **Buy a dongle.** The M.2 card is behind three unbuilt prerequisites
   (booting a real PC at all, PCIe interrupts beyond legacy INTx, and a PCI
   layer without extended config space), and QEMU cannot emulate Wi-Fi. A USB
   dongle plugs straight into what exists: QEMU's `usb-host` passthrough
   hands a real dongle to the guest, and LazyOS already drives `qemu-xhci`.
   The recommended dongle is an **MT7925-based USB adapter** (Netgear A9000
   or A8500, about $85–100): it runs the *same* `mt7925/` code and firmware
   as the M.2 card, so everything learned transfers, and only the bus glue
   (`pci.c`, 937 lines, vs `usb.c`, 372 lines) changes later. The budget
   alternative is an **MT7921AU** dongle (Comfast CF-953AX class, $15–25),
   which shares the `mt792x` and `connac` MCU libraries but has its own MAC
   descriptors and MCU command set, so roughly two thirds transfers.
5. **No relicensing is needed for this path.** Relicensing to
   GPL-2.0-or-later (which the single copyright holder can do) would unlock
   `mac80211` and the GPL-only dongle drivers, but it would create a new
   conflict: the Droid fonts compiled into every `xui` binary are Apache-2.0,
   which is GPLv2-incompatible. §4 gives the options; the recommendation is
   to stay GPL-3.0-or-later and extract only ISC/BSD/Clear-BSD/`OR BSD`
   code, which is exactly what the MT7925 path requires.

## 2. Facts (verified 2026-10-02)

### 2.1 The Linux MT7925 driver

Checked out at `torvalds/linux` master `ce1e0223d8ad` (2026-10-01).

| Build unit | Files | Lines | Licence |
|---|---|---|---|
| `mt7925/` (chip) | `mcu.c` 4,174, `main.c` 2,794, `mac.c` 1,737, `nan.c` 1,098, `pci.c` 937, `usb.c` 372, headers, `regd.c`, `init.c`, `debugfs.c`, `testmode.c` | 14,545 | BSD-3-Clause-Clear |
| `mt792x-lib` (+usb) | `mt792x_core.c` 1,172, `mt792x_dma.c` 626, `mt792x_usb.c` 531, `mt792x_mac.c` 384, `mt792x_acpi_sar.c` 487, headers | ~4,745 | BSD-3-Clause-Clear |
| `mt76-connac-lib` | `mt76_connac_mcu.c` 3,568, `mt76_connac_mac.c` 1,230, `mt76_connac3_mac.c` 268, headers | ~8,527 | BSD-3-Clause-Clear |
| `mt76` core | `mac80211.c` 2,376, `dma.c` 1,237, `tx.c` 1,027, `mt76.h` 2,167, `mmio util trace eeprom agg-rx mcu scan channel` | ~9,626 | BSD-3-Clause-Clear |
| `mt76-usb` | `usb.c` 1,202 | ~1,300 | BSD-3-Clause-Clear |
| **USB MT7925 total** | | **≈ 39 kLOC** | |

Of 255 tagged files in `mt76/`, 217 are BSD-3-Clause-Clear and 18 are
GPL-2.0-only: all of `mt76x0/` (the **MT7610U** driver, deliberately kept GPL
in the relicensing commit), `npu.c`, `mt7996/npu.c`, `mt7615/usb_mcu.c`,
`mt7615/sdio_mcu.c`. None is on the MT7925 or MT7921 path. Modules declare
`MODULE_LICENSE("Dual BSD/GPL")`.

Dependencies by `#include` census: `<net/mac80211.h>`, `<net/cfg80211.h>`,
`<linux/ieee80211.h>`, `<linux/firmware.h>`, `<linux/dma-mapping.h>`,
`<linux/skbuff.h>`, `<linux/usb.h>`, `<linux/pci.h>`, workqueues and delayed
work, kthreads, debugfs, devcoredump, leds, thermal, hwmon, ACPI, page pool,
nvmem, of, mtd, regmap, tracepoints, WED, SDIO. The optional ones (debugfs,
leds, thermal, hwmon, ACPI SAR, NAN, testmode, WED, SDIO, MLO) are a third of
the code and are the first thing to drop.

**What the firmware does vs the host** (from `mt7925_ops` in `mt7925/main.c`
and `mt792x_init_wiphy`):

| Offloaded to firmware / MCU | Done by the host (mac80211 today) |
|---|---|
| Scanning (`hw_scan`, scheduled scan, all bands) | Building and parsing auth/assoc/deauth frames, the STA state machine |
| Rate control (`HAS_RATE_CONTROL`) | BSS selection, timers, roaming decisions |
| **802.3 ↔ 802.11 encap/decap** (`SUPPORTS_TX/RX_ENCAP_OFFLOAD`) | **EAPOL / WPA 4-way handshake** (`NL80211_EXT_FEATURE_4WAY_HANDSHAKE_*` appears nowhere in `mt76`) |
| Power save, beacon loss, keepalive (`CONNECTION_MONITOR`) | Key material (then installed via MCU `STA_REC_KEY_V3`; WEP, TKIP, CCMP(-256), GCMP(-256), BIP all in hardware) |
| A-MPDU/A-MSDU, reordering, WoWLAN, CSA, remain-on-channel | Regulatory domain choice (tables in `regd.c`) |

Two consequences shape the design. The data path can be **plain Ethernet
frames** at the driver boundary, so the existing `os.lazy.net.nic.v1` ring
interface fits unchanged. And the firmware does not do the handshake, so
LazyOS needs its own small supplicant (§5.4).

### 2.2 Firmware

`mediatek/mt7925/WIFI_RAM_CODE_MT7925_1_1.bin` (1,392,088 B) and
`mediatek/mt7925/WIFI_MT7925_PATCH_MCU_1_1_hdr.bin` (188,192 B) in
linux-firmware, `LICENSES/LICENCE.mediatek`: MediaTek "grants permission to
use and redistribute aforementioned firmware files for the use with devices
containing MediaTek chipsets, **but not as part of the Linux kernel or in any
other form which would require these files themselves to be covered by the
terms of the GNU General Public License**". So: separate files on the OS
volume (never `include_bytes!`), the licence text next to them, fetched at
build with a SHA-256 pin, exactly the `tools/doom/fetch.py` pattern. MT7921:
`WIFI_RAM_CODE_MT7961_1.bin` (792,036 B) + `WIFI_MT7961_patch_mcu_1_2_hdr.bin`.

### 2.3 The GPL-2.0-only wall and what is on the other side

- `net/mac80211/`: 92,632 lines, 93 files GPL-2.0-only (one ISC).
  `net/wireless/` (cfg80211): 55,991 lines, 38 files GPL-2.0-only (3 ISC).
  Headers `mac80211.h` 8,232, `cfg80211.h` 11,022, `ieee80211.h` 2,903, all
  GPL-2.0-only. The FSF: "there is no legal way to combine code under GPLv2
  with code under GPLv3 in a single program".
- **OpenBSD `sys/net80211/`**: 24,693 lines, 17 files ISC + 14 files BSD.
  `ieee80211_pae_input.c` (1,198) + `ieee80211_pae_output.c` (640) implement
  "the 4-Way Handshake and Group Key Handshake protocols (both Supplicant and
  Authenticator Key Receive state machines)". OpenBSD does WPA2-PSK **in the
  kernel, without wpa_supplicant** (which it needs only for 802.1X/EAP).
  Also: STA MLME, scanning, roaming, CCMP/TKIP/WEP/BIP software fallbacks,
  HT/VHT rate adaptation. This is the right-sized extraction source.
- **FreeBSD** `sys/net80211/`: 65,030 lines, BSD-2/3-Clause; no in-kernel
  supplicant. `sys/compat/linuxkpi/` 802.11 part (`linux_80211.c` 10,309 +
  `linux_80211_macops.c` 1,041 + the two shim headers) ≈ 16.4 kLOC,
  BSD-2-Clause: a worked, licence-clean example of a `mac80211`-shaped shim.
  FreeBSD 16 ships `mt7921(4)` ("derived from MediaTek's Linux mt76 driver
  based on Linux version 7.0", STA only, a/b/g/n/ac today) and builds
  `sys/modules/mt76/mt7925` with `MT7925_USB=1`, without a man page yet.
- **hostap** (wpa_supplicant): BSD-3-Clause since 2012. A PSK-only 4-way +
  group handshake with its primitives (`sha1-prf`, `sha1-pbkdf2`,
  `aes-unwrap`, `aes-omac1`) is ~2–4 kLOC. **iwd** is LGPL-2.1-or-later
  (compatible, but welded to nl80211). *Unverified*: Fuchsia's Rust
  `wlan-rsn` / `wlan-mlme` / `wlan-sme` (BSD-3-Clause) implement exactly
  MLME + RSNA supplicant in Rust; `fuchsia.googlesource.com` was unreachable
  from this sandbox. Worth one afternoon of reading before W0 starts.

### 2.4 USB dongle candidates

| Chipset | Linux driver | Linux licence | Firmware | BSD native driver | Radio | Example / price |
|---|---|---|---|---|---|---|
| **MT7925** (USB) | `mt7925u` (6.7+), ≈39 kLOC path | BSD-3-Clause-Clear (ISC ≤ 6.18) | 1.39 MiB + 188 KiB, `LICENCE.mediatek` | FreeBSD builds it (LinuxKPI); OpenBSD none | be/ax/ac, 2.4/5/6 GHz, 2×2, 160 MHz | Netgear A9000 `0846:9072` ~$85–100, A8500 `0846:9050` |
| **MT7921AU** | `mt7921u` (5.18+), `mt7921/` 7,798 + same libs | BSD-3-Clause-Clear | 792 KiB + patch, `LICENCE.mediatek` | FreeBSD PCIe only; OpenBSD none | ax/ac, 2.4/5 (6) GHz, 2×2 | Comfast CF-953AX ~$15–25; Netgear A8000 `0846:9060` ~$60–100 |
| MT7612U | `mt76x2u`, 3,855 + `mt76x02` 5,600 | BSD-3-Clause-Clear | 91 KiB + 20 KiB | none | ac, dual band, 2×2 | Alfa AWUS036ACM `0e8d:7612` ~$40–47; Netgear A6210 |
| AR9271 | `ath9k_htc`, ≈8 kLOC of 88k `ath9k` | ISC (no SPDX tags; ISC text) | 51 KiB, **free software** (`open-ath9k-htc-firmware`, Clear-BSD/MIT/3 GPLv2 files) | OpenBSD `athn(4)` ISC, 5.9 kLOC | n, **2.4 GHz only**, 1×1 | Alfa AWUS036NHA (EOL) $35–70; TL-WN722N **v1 only** |
| RT5370 / RT3070 | `rt2800usb` (`rt2x00`, 46.9 kLOC) | **GPL-2.0-or-later** (usable under GPLv3) | 8 KiB `rt2870.bin` | OpenBSD/FreeBSD `run(4)` ISC, 4.8/6.5 kLOC | n, 2.4 GHz, 1×1 | Panda PAU05 ~$19; generic ~$8 |
| RTL8812AU / 8821AU | `rtw88` (USB since 6.2; 8812au since 6.13) | **GPL-2.0 OR BSD-3-Clause** | 27–139 KiB, `LICENCE.rtlwifi_firmware` | FreeBSD `rtwn_usb(4)` ISC | ac, dual band | Alfa AWUS036ACH ~$50–65 |
| MT7601U | `mt7601u`, 7,794 | **GPL-2.0-only** ✗ | 45 KiB | OpenBSD/FreeBSD `mtw(4)` ISC | n, 2.4 GHz | $5–9 sticks |
| MT7610U | `mt76x0u` | **GPL-2.0-only** ✗ | 80 KiB | `mtw(4)` code paths, undocumented | ac, 1×1 | Alfa AWUS036ACHM ~$49 |
| RTL8188EU / 8192EU | `rtl8xxxu`, 24.4 kLOC | **GPL-2.0-only** ✗ | 15–32 KiB | OpenBSD `urtwn(4)` ISC 2.6 kLOC, FreeBSD `rtwn(4)` | n, 2.4 GHz | TL-WN725N ~$5–8 |

Prices are retail search results, *unverified*. ✗ marks chips whose Linux
driver cannot be used under GPL-3.0-or-later; their OpenBSD drivers can.

### 2.5 Testing reality

- **QEMU emulates no wireless NIC** (`hw/net/meson.build` lists only wired
  models). `mac80211_hwsim` and `virt_wifi` are Linux modules, not QEMU
  device models; nothing simulates a radio for a non-Linux guest.
- **USB passthrough works**: `-device qemu-xhci -device
  usb-host,vendorid=0x0846,productid=0x9072` (or `hostbus`/`hostaddr`, or
  `hostport` to survive re-plugs) on a KVM Linux host with the host driver
  unbound. QEMU calls it experimental; it needs KVM, not TCG.
- **PCIe passthrough** of the M.2 card needs VFIO: an IOMMU on in firmware
  and kernel, the card's whole IOMMU group bound to `vfio-pci`, and the host
  loses its Wi-Fi while the guest has it. Laptop M.2 slots often share a
  group. Possible, not pleasant.

## 3. Where LazyOS is (prerequisites and gaps)

| Piece | State today | What Wi-Fi needs |
|---|---|---|
| Network stack | N0–N5 built: `netdrv` (virtio-net) → `os.lazy.net.nic.v1` shared frame rings → `netd` (smoltcp) → sockets, Linux `AF_INET` shim | Nothing new at the frame boundary: `nic.v1` is pure 802.3 + MAC + link state, and the plan always meant a second driver to serve it unchanged |
| Multiple NICs | One: registry name `os.lazy.net.nic` is a constant, `netd` hardcodes `eth0`, one smoltcp `Interface`, driver accepts one client | A `wlan0` beside `eth0`: class-based registry names, N interfaces in `netd`, a route choice. Listed as a §12 non-goal of the net plan; becomes stage N7 |
| USB host | `usbd` + `libs/xhci`: control and interrupt-IN only, root ports only (no hubs), polling (no MSI), HID-specific end to end, no Messenger interface, one owner per controller | **Bulk IN/OUT** (`EndpointType::Bulk*` exists, never used); a way for a second process to reach a device (§5.2) |
| PCI / DMA | Syscall 23 `claim`/`map_bar`/`dma_alloc`; legacy INTx only; 256-byte config space; DMA ≤ 4 MiB per buffer, 16 buffers, 8 MiB per uid, below 4 GiB, no scatter-gather, no IOMMU; never free DMA while the device runs | Fine for a USB dongle (the xHCI driver already lives inside it). The M.2 card wants MSI (`mt76` falls back to INTx, *unverified* on this chip), PCIe capability writes (ASPM), and a real PC |
| Real hardware | Not booted on any PC; [real-pc-boot-plan.md](real-pc-boot-plan.md) H0–H4 are a draft | Required for the M.2 card, not for a dongle in QEMU |
| Secrets | `keyd` holds keys in memory only, no persistence, no "store a named secret and use it"; `libs/crypto` has argon2, hkdf, hmac, sha2 | Persistent per-user secrets; **SHA-1** (PBKDF2 and the WPA2 PRF), **AES** key-unwrap (GTK delivery), AES-CMAC (WPA2 with SHA-256 AKMs, later) |
| C in the OS | Only in Linux-ABI musl programs (doomgeneric, litehtml, BusyBox), via zig; the net plan rejected lwIP because a C parser of hostile input "contradicts the code standards" | Decides §5.5: rewrite in Rust vs vendor `mt76` C behind a shim |
| Firmware blobs | No precedent; nearest is Freedoom inside the Doom `.lzp` | A fetched, hash-pinned, separately licensed file set on the OS volume under an `fhs` path |
| Launchers | `LAZYOS_NET`, `LAZYOS_USB`, `LAZYOS_SOUND` env switches; `run_demo.py` has `--net`/`--sound` only; GUI catalog has sound only | `LAZYOS_WIFI=1`, `run_demo.py --wifi --usb-host VID:PID`, a GUI control (AGENTS.md rule) |

## 4. The licence decision

LazyOS is GPL-3.0-or-later with a single copyright holder, so any of these is
a one-line decision. The commits co-authored by an assistant carry no
separate copyright.

**A. Stay GPL-3.0-or-later; extract only compatible code — recommended.**
What may be taken: ISC, BSD-2/3, BSD-3-Clause-Clear, MIT, `GPL-2.0 OR
BSD-3-Clause` (taken under BSD), **GPL-2.0-or-later** (taken under GPLv3),
LGPL-2.1-or-later. That covers the entire MT7925/MT7921/MT7612U path, `rtw88`,
`ath9k_htc`, `rt2x00`, all of OpenBSD and FreeBSD `net80211`, FreeBSD's
LinuxKPI, and hostap. What may not: `mac80211`, `cfg80211`, `mt7601u`,
`mt76x0`, `rtl8xxxu`. None of the excluded code is wanted. The one working
rule it imposes: **never read GPL-2.0-only code "for reference" while writing
our own**; use the permissive trees (mt76, OpenBSD, FreeBSD LinuxKPI) as the
specification instead. They are sufficient.

**B. Relicense the OS to GPL-2.0-or-later.** Unlocks all of Linux, and the
combination with any GPL-2.0-only file becomes effectively GPLv2-only. Costs:
the Droid Sans fonts are `include_bytes!`'d into `xui-app` (Apache-2.0, which
the FSF lists as **incompatible with GPLv2**), `doom/NOTICE` would need
rewriting, and every Cargo dependency would need re-auditing for
Apache-2.0-only crates (the README's "GPLv3-compatible" claim says nothing
about GPLv2; there is no `cargo deny` in the tree to check it). Buys nothing
for the recommended path.

**C. Per-program licensing.** Keep the OS GPL-3.0-or-later and, if a
GPL-2.0-only driver is ever wanted, ship that driver as its own program under
GPL-2.0 talking to the rest over Messenger. The FSF treats pipes and sockets
between separate programs as "mere aggregation" unless the semantics are
"intimate enough, exchanging complex internal data structures". A frame ring
of Ethernet packets behind a MIDL-defined interface is about as un-intimate as
IPC gets, and this is the same reasoning `LICENCE.mediatek` relies on for the
firmware. A gray area the project does not need to enter now; noted as the
escape hatch.

**D. Go permissive (MIT/BSD).** Then no GPL code at all, ever. Loses the
`rt2x00` option and nothing else on this page; a decision for other reasons
than Wi-Fi.

Recommendation: **A**, recorded in `README.md` §License and a new
`THIRD_PARTY.md` listing every extracted file with its origin commit and
licence, because this will be the first time LazyOS carries derived code
inside a native service rather than a bundled program.

## 5. Architecture

```
                 ┌──────────── unprivileged (no caps) ────────────┐
   netd ◄── nic.v1 rings ──► wifid ◄── wifi.v1 ──► wlanmd ◄── keyd.v1 ──► keyd
 (smoltcp,        802.3        (bus + MCU +        (MLME, scan           (PSK/PMK,
  eth0+wlan0)                   firmware; DMA,      parsing, 4-way        persisted,
                                CAP_DEV_CLAIM)      handshake, keys)      sealed)
                                   │
                        libs/xhci bulk pipes (W1: own controller;
                        W5: usbd os.lazy.usb.device.v1)
                                   │
                              USB dongle  ──air──►  AP  (hostapd + tcpdump on the dev host)
```

### 5.1 Processes and trust

Mirror the `netdrv`/`netd` split: the process that holds DMA parses nothing
from the air; the process that parses hostile input holds no capabilities.

- **`wifid`** (uid `_wifi` = 906, `CAP_DEV_CLAIM`, class `os.kernel.dev.usb`
  in W1, `os.kernel.dev.net` for the M.2 card later, since PCI class 0x0280
  already maps to `net`). Owns the bus, firmware download, the MCU command
  and event protocol, hardware key slots and the TX/RX rings. Serves
  `os.lazy.net.nic.v1` **unchanged** towards `netd` (encap offload makes the
  frames Ethernet) and a thin `os.lazy.net.wifi.hw.v1` towards `wlanmd`
  (start scan, raw management frame in/out, set key, set BSS/channel,
  firmware events). It never looks inside a beacon.
- **`wlanmd`** (uid 907, no caps). The station MLME: scan-result parsing
  (beacons, probe responses, information elements), BSS selection, the
  auth/assoc state machine, the EAPOL 4-way and group handshakes, roaming
  and reconnect timers. Serves **`os.lazy.net.wifi.v1`** to the system:
  `Scan`, `Networks`, `Connect(ssid, key_ref)`, `Disconnect`, `Status`,
  topics `system/net/{nic}/wifi/state` (retained) and
  `system/net/{nic}/wifi/scan`. Every parser in it is a fuzzed `no_std`
  library (`libs/ieee80211`, `libs/eapol`), like `usbhid` and `netstack`.
- **`keyd`** gains persistence and an 802.11 PSK operation: store a
  passphrase (or the derived PMK) sealed per user; derive the PTK for a
  handshake and return it only to `wlanmd`. The temporal key must reach the
  hardware anyway, so v1 lets `wlanmd` hold KCK/KEK/TK in memory for the
  session; moving the MIC and unwrap steps into `keyd` so `wlanmd` never
  sees the KEK is a later tightening (the platform plan's "TLS terminated
  via `keyd`" pattern).
- **`wifictl`** CLI (`scan`, `connect`, `status`), and a Settings page later.

### 5.2 Reaching the dongle

One device has one owner, and the keyboard and mouse share the only xHCI
controller with the dongle. Two ways out, used in sequence:

- **W1 shortcut:** QEMU gets a second controller, `-device qemu-xhci,id=wifi`
  with the `usb-host` device on it, and `wifid` claims that controller
  directly with `libs/xhci` (now with bulk support). No new interface, no
  change to `usbd`, fast start. Only a development rig, since a real PC has
  one controller.
- **W5:** `usbd` exports `os.lazy.usb.device.v1` (MIDL): descriptors, control
  transfers as calls, bulk and interrupt pipes as shared rings in the
  `framering` style, hot-plug as topics. `wifid` becomes a class driver on
  it, and so can a future USB mass storage or audio class driver
  ([real-pc-boot-plan.md](real-pc-boot-plan.md) H4 asks for the same thing).

### 5.3 Data path

Firmware encap offload means `wifid` moves Ethernet frames between `nic.v1`
rings and the USB bulk endpoints, prefixing MediaTek's TXD/RXD descriptors
(`mt76_connac3_mac`). No 802.11 header handling in the data path at all;
A-MSDU/A-MPDU, reordering and rate control stay in the chip. The 2046-byte
slot cap of `nic.v1` fits a 1500-byte MTU; 802.11 jumbo A-MSDU is irrelevant
after decap. Throughput with polled bulk transfers at 1 ms will be a few
MB/s, enough for v1; interrupt-driven xHCI is the N6/MSI work, not Wi-Fi's.

### 5.4 Control plane: what has to be written

| Component | Source of truth | Est. Rust |
|---|---|---|
| Firmware download (patch semaphore, RAM code, `FW_START`) | `mt76_connac_mcu.c` (`mt76_connac2_load_patch/ram`), `mt792x_usb.c` | 0.6 k |
| MCU command/event framing, TLV station records, scan, BSS, channel, key, power | `mt7925/mcu.c`, `mt76_connac_mcu.c`, `mt7925/mcu.h` structs | 3–4 k |
| USB bus: bulk rings, descriptor prefix, `mt76u_*` register access over vendor control requests | `mt76/usb.c`, `mt792x_usb.c`, `mt7925/usb.c` | 1.5 k |
| TXD/RXD descriptors, status | `mt7925/mac.c`, `mt76_connac3_mac.h` | 1 k |
| IE/beacon/management parsing (fuzzed lib) | OpenBSD `ieee80211_input.c`, `ieee80211_node.c`; IEEE 802.11-2020 | 1.5 k |
| STA state machine (scan → auth → assoc → run → roam) | OpenBSD `ieee80211_proto.c`, `ieee80211_node.c` | 1.5 k |
| EAPOL 4-way + group handshake, PBKDF2-SHA1, PRF, AES key unwrap, MIC | OpenBSD `ieee80211_pae_{input,output}.c` (1,838 lines ISC); hostap `wpa_common.c` for cross-checking | 1.5 k + 0.5 k crypto |
| `keyd` persistence and the PSK op | security-model §8 | 0.8 k |
| `netd` multi-interface, `wifictl`, MIDL, `init` rows, build switches, launchers | existing patterns | 1.5 k |
| **Total** | | **≈ 13–15 kLOC** |

### 5.5 Rewrite in Rust, or vendor the C?

Two defensible routes; the plan takes the first.

- **Rust rewrite with `mt76` as the specification (chosen).** The
  hardware-facing part is ~7 kLOC of protocol code (firmware load, MCU TLVs,
  USB framing, descriptors) with the Linux driver as a complete, permissively
  licensed, executable specification; FreeBSD's successful port proves the
  driver has no hidden dependency on Linux internals that matters. Every
  struct keeps a comment naming the `mt76` file, function and pinned commit
  it was derived from, and `THIRD_PARTY.md` carries the Clear-BSD notice,
  because a close translation is a derived work and should say so. It keeps
  the kernel-adjacent code within the project's standards (no `unsafe` C
  parsing firmware events; the one `unsafe` surface stays the DMA ring).
- **Vendor `mt76` C behind a LinuxKPI-like shim.** Compile the driver
  unmodified with zig, as Doom compiles doomgeneric, inside a Rust `std`
  (Linux-ABI musl) process, and emulate `skb`, workqueues, `usb_submit_urb`,
  `request_firmware` and a `mac80211`-shaped callback surface. FreeBSD's
  numbers say what that costs: the 802.11 shim alone is 16 kLOC, more than
  the rewrite, before `skb`/USB/work emulation, and the result is 39 kLOC of
  C in a system whose networking plan refused 20 kLOC of lwIP. It also needs
  a Linux-ABI process to reach syscall 23 and Messenger (*unverified*:
  Doom's musl build reaches Messenger through the native pump, so this is
  plausible). Its merit is fidelity: fewer MCU protocol mistakes on day one.
  Keep it as the fallback if the rewrite stalls on an undocumented firmware
  behaviour; the rig and the frame interface are identical either way.

## 6. Hardware recommendation

| Option | Pros | Cons | Verdict |
|---|---|---|---|
| **MT7925 USB** (Netgear A9000 / A8500, ~$90) | Same `mt7925/` code, same firmware and MCU dialect as the M.2 card; a Wi-Fi 7 radio; USB bus glue is 372 lines; FreeBSD builds the same combination | Price; a newer firmware whose MLO/NAN/6 GHz features we will ignore | **Buy this** if the M.2 card is the real goal |
| MT7921AU (Comfast CF-953AX, ~$20) | Cheapest `mt792x`/`connac` path; same USB glue and firmware loader; Wi-Fi 6 | Different MAC descriptors (`connac2` vs `connac3`) and MCU command set (`mt7921/mcu.c` vs `mt7925/mcu.c`): ~⅔ transfers | Buy this if $90 is too much; plan a `mt7925` port as a second chip |
| AR9271 (AWUS036NHA / TL-WN722N v1) | Simplest protocol (~8 kLOC), **fully open firmware**, ISC driver, OpenBSD `athn` as an independent second reference | 2.4 GHz 802.11n only, EOL and counterfeited; nothing transfers to the MT7925 | The pedagogical choice, not the goal |
| RT5370 (~$8) | Cheapest; register-level chip with 8 KiB firmware; OpenBSD `run(4)` ISC | 2.4 GHz n only; nothing transfers | A "first light" toy if a second driver is acceptable; skipped |
| MT7925 M.2 via VFIO now | The actual device | Needs IOMMU and group isolation, loses host Wi-Fi, still blocked on INTx-only PCI and the missing real-PC path for anything beyond QEMU | W6, after H0–H3 |

### 6.1 Buying one in France (snapshot 2026-10-02)

Chipsets below were verified against the USB ID tables in `mt7925/usb.c`
and `mt7921/usb.c` and the `morrownr/USB-WiFi` adapter list; prices are
same-day retailer snapshots and will drift.

| Product | Chip, USB ID | Driver | Seen at | Snapshot price |
|---|---|---|---|---|
| **Netgear Nighthawk A8500** (BE5000) | MT7925, `0846:9050` | `mt7925u` (ID added to mainline 2026-03, backports to 6.12–6.18 stable) | amazon.fr, Fnac and Darty marketplace | ~68–70 € |
| **Netgear Nighthawk A9000** (BE6500) | MT7925, `0846:9072` | `mt7925u` (ID in 6.18, backported to 6.12) | LDLC (first-party, in stock), amazon.fr, materiel.net | 150 € at LDLC; a 76 € amazon.fr offer looked like second-hand stock |
| **BrosTrend AX9L** (AXE3000) | MT7921AU, `0e8d:7961` | `mt7921u` (generic ID, 5.18+) | amazon.fr | ~40 € |
| Netgear Nighthawk A8000 (AXE3000) | MT7921AU, `0846:9060` | `mt7921u` (6.4+) | amazon.fr, Fnac, Darty, LDLC, materiel.net, TopAchat | 65–113 € |
| Alfa AWUS036AXML | MT7921AUN, `0e8d:7961` | `mt7921u` (6.12+ for the BT combo) | getic.fr, amazon.fr | 52–59 € |
| TP-Link Archer **TXE50UH** | MT7921AU, `35bc:0107` | `mt7921u` (6.14+) | pc21.fr, Fnac marketplace | 48–68 € |

**Wrong chip, do not buy for this:** TP-Link Archer TX20U, TX20U Plus,
TX20UH and TBE400UH, ASUS USB-AX56, D-Link DWA-X1850, BrosTrend AX1L/AX4L
are all Realtek (`rtw89` or out-of-tree `rtl8852au`) despite the similar
names. Comfast CF-953AX is MediaTek but discontinued and was dropped from
the plug-and-play list for USB resets during its Bluetooth firmware load.
The A9000 has a known warm-reboot quirk (needs a replug after a reboot).

Pick: the **A8500** is the cheapest way to the exact `mt7925/` code; the
**AX9L** is the cheapest MediaTek stick of any kind.

## 7. Stages

Letters W. Each stage ends with evidence captured by a harness, as in every
other plan: the verdict is what crossed the air (tcpdump on the AP side of a
host `hostapd`) and what `netd` could do over `wlan0`, never a serial marker.

| Stage | Deliverable | Depends on | Evidence |
|---|---|---|---|
| **W0 Groundwork** | Licence decision recorded (§4); dongle bought; `THIRD_PARTY.md`; `libs/ieee80211` (frames, IEs) and `libs/eapol` with seeded fuzz; `libs/crypto` gains SHA-1, AES, AES key-wrap; firmware fetch script (`tools/wifi/fetch.py`, SHA-256 pinned, `LICENCE.mediatek` alongside) and an `fhs` firmware path; verify the Fuchsia `wlan-rsn` licence and shape | nothing | `cargo test` on the new libs; `python fuzz/gen_corpus.py --check` |
| **W1 Bus and firmware** | `libs/xhci` bulk IN/OUT; `wifid` claims a dedicated second `qemu-xhci`, enumerates the dongle, downloads patch + RAM code, reads the EEPROM/MAC | W0; `LAZYOS_WIFI=1`; `run_demo.py --wifi --usb-host VID:PID` | `tools/wifi/run.py --fw`: firmware `READY` event and MAC address in the log; the dongle's LED |
| **W2 MCU and scan** | Connac MCU framing, station-record TLVs, `hw_scan`; `wlanmd` parses results; `wifictl scan` lists SSIDs with RSSI and channel | W1 | the host `hostapd` SSID appears with the right channel |
| **W3 Open association and first packets** | Auth/assoc state machine, BSS and channel set via MCU, encap offload on, `nic.v1` served, `netd` attaches a second interface `wlan0` | W2; N7 (multi-NIC `netd`) | DHCP lease and `ping` over `wlan0` through an open test AP; AP-side pcap shows the frames |
| **W4 WPA2-PSK** | 4-way and group handshakes in `wlanmd`, PMK from `keyd` (persisted), CCMP keys installed via `STA_REC_KEY_V3`, rekey, deauth handling | W3 | DHCP + `ping` through a WPA2 AP; AP-side pcap shows protected frames; a wrong passphrase fails cleanly and is logged |
| **W5 Productization** | `usbd` exports `os.lazy.usb.device.v1` and `wifid` moves onto it (single controller); reconnect and roaming basics; `wifictl status`; Settings page; GUI catalog control and tests; `docs/architecture/wifi.md` | W4 | `tools/wifi/run.py` full pass on one `qemu-xhci` with keyboard, mouse and dongle; `tools/usb/run.py` still green |
| **W6 MT7925 M.2** | `pci.c` + `mt792x_dma.c` port (~1.7 kLOC): PCIe DMA rings within the 4 MiB / 16-buffer limits, INTx (or MSI when the device core has it), ASPM off | real-pc-boot H0–H3; `wifid` on `os.kernel.dev.net` | the laptop joins the home network from the USB stick image |

Deliberately later or out: WPA3-SAE (needs P-256 ECC in `libs/crypto`),
802.1X/EAP, AP and mesh modes, 6 GHz regulatory handling beyond a country
code, MLO, NAN, WoWLAN, Bluetooth on the combo chip, TKIP/WEP (hardware does
them; the MLME refuses them).

## 8. Verification

- **Host tests and fuzz** (CI): every parser (`ieee80211`, `eapol`, MCU event
  decoding, descriptor decoding) is a `no_std` library with a `fuzz::run`
  entry shared with a cargo-fuzz target; the handshake is tested against the
  IEEE 802.11 Annex J test vectors and against a recorded `hostapd` exchange.
- **Live harness** (`tools/wifi/run.py`, not CI): a Linux dev host with KVM,
  the dongle unbound from its host driver and passed through, a second
  adapter (or the home AP) running `hostapd` with `tcpdump` on its interface.
  The judge reads the AP-side pcap (association, EAPOL 1–4, protected data,
  DHCP, ICMP) and the guest's `netctl`/`ping` output. The same `--services`,
  `--restart` and `--hotplug` variants as the USB and net harnesses: `wifid`
  killed mid-association must be restarted by `init` and re-associate without
  the controller stalling.
- **Nothing in QEMU alone proves Wi-Fi.** A "software dongle" that speaks the
  MCU protocol to a fake radio would exercise `wifid` in CI; it is listed as a
  possible later tool, not a stage.

## 9. Risks and open questions

1. **The MCU protocol is the product.** Firmware-defined, versioned by the
   firmware files, documented only by the driver. A firmware update can
   change TLV versions (the `_V3` suffixes are the evidence). Mitigation: pin
   firmware files and the `mt76` reference commit together; port the version
   negotiation, not just the happy path.
2. **Translation fidelity.** A Rust rewrite of 7 kLOC of C against a chip
   with no datasheet will have mistakes that the C does not. Mitigation: the
   vendored-C fallback (§5.5) stays available; FreeBSD's `mt7921`/`mt7925`
   build is a second permissive reference for which code paths matter.
3. **USB passthrough under QEMU** is "experimental" and slow; it needs KVM
   and a Linux host, and `hostport` to survive re-plugs. On the Windows
   hosts the tooling also supports, `usb-host` needs a libusb-compatible
   driver bound to the dongle and is untested here.
4. **Derived-work discipline.** Clear-BSD requires the notice and forbids
   implying endorsement by MediaTek; Linux's history is GPL even where a
   file is not. Only read permissive trees; record origin per file.
5. **Secrets.** Until `keyd` persists and seals, a passphrase would be
   re-entered each boot. W4 pulls the `keyd` persistence work forward from
   the security plan.
6. **Regulatory.** The firmware enforces a regulatory domain from `regd.c`
   tables; v1 sets one country code from `confd` and refuses to transmit
   without it.
7. **Did FreeBSD's `mt7925u` actually associate?** Compiled, undocumented,
   unknown. If their mt7925 USB path is broken, we find the bugs. The
   Netgear A9000 is listed by Linux, which is the reference that matters.
8. **Is Fuchsia's Rust RSNA worth taking over porting OpenBSD's C?** Decided
   in W0 after reading it; either is licence-clean.

## 10. Non-goals

Anything other than station mode on one adapter; `nl80211`, `wpa_supplicant`
or `iw` compatibility through the Linux ABI (no netlink exists and none is
planned); porting `mac80211`; a generic Linux driver compatibility layer
(FreeBSD's LinuxKPI is a reference, not a goal); Bluetooth; using the Wi-Fi
work to drive the real-PC boot plan (it is the other way round).

## 11. Decisions requested

1. Licence: option **A** (stay GPL-3.0-or-later, extract permissive and
   GPL-2.0-or-later code only).
2. Hardware: an **MT7925 USB adapter** (Netgear A9000 `0846:9072` or A8500
   `0846:9050`), or the MT7921AU dongle as the budget route.
3. Method: **Rust rewrite with `mt76` as the specification**, vendored C as
   the fallback.
4. Ordering: W0 can start now on host-only libraries; W1 waits for the
   dongle; W3 waits for multi-NIC `netd` (N7), which is small and useful on
   its own.
