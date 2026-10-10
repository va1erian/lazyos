# Third-party code and data

LazyOS is GPL-3.0-or-later ([`LICENSE`](LICENSE)). This file is the register
of third-party material that is **taken into the tree or the image** and that
needs a licence decision: vendored or derived source, and binary blobs. It
exists because the Wi-Fi work ([`docs/wifi-plan.md`](docs/wifi-plan.md) section
4, option A) will be the first time a native service carries code derived from
another project.

Not repeated here, because they already have a home:

- Cargo dependencies: every crate is MIT, Apache-2.0, BSD, Zlib or Unlicense
  (see the README's License section); the TLS stack's tree is enforced by
  `python tools/nettls/licenses.py`. Crypto crates are listed below only
  because `libs/crypto` is where they are chosen.
- Fonts, samples and wallpapers: one line each in
  [`assets/manifest.txt`](assets/manifest.txt), licence texts beside them.
- Doom, Freedoom and doomgeneric: [`doom/NOTICE`](doom/NOTICE).

## The rule

Code may be copied or derived from only if its licence can be absorbed by
GPL-3.0-or-later:

| Accepted | Taken under |
|---|---|
| ISC, BSD-2-Clause, BSD-3-Clause, BSD-3-Clause-Clear, MIT | as is |
| `GPL-2.0 OR BSD-3-Clause` (dual) | the BSD side |
| GPL-2.0-or-later, LGPL-2.1-or-later | GPLv3 |
| `MIT OR Apache-2.0` (Rust crates) | MIT |

Refused: **GPL-2.0-only** code (Linux `mac80211`, `cfg80211`, `mt7601u`,
`mt76x0`, `rtl8xxxu`, ...), Apache-2.0-only code, and anything without a
licence. The working rule that follows: **never read GPL-2.0-only code "for
reference" while writing our own.** Where Linux is the only place a behaviour
is documented, use the permissive trees (the `mt76` driver, OpenBSD and
FreeBSD `net80211`, hostap) as the specification.

Firmware blobs are not source and are never linked into a binary or
`include_bytes!`'d (that is what their licences forbid); each is a file in the
image under its own licence text. Every entry below records its origin
(project and commit or version), its licence and the files it touches.

## Register

| Component | Origin | Licence | Files |
|---|---|---|---|
| `sha1` 0.10.6 | RustCrypto, crates.io | MIT OR Apache-2.0, taken under MIT | `libs/crypto` (dependency; soft backend) |
| `pbkdf2` 0.12.2 | RustCrypto, crates.io | MIT OR Apache-2.0, taken under MIT | `libs/crypto` (dependency) |
| `aes` 0.8.4 | RustCrypto, crates.io | MIT OR Apache-2.0, taken under MIT | `libs/crypto` (dependency; `aes_force_soft`, see `.cargo/config.toml`) |
| `aes-kw` 0.2.1 | RustCrypto, crates.io | MIT OR Apache-2.0, taken under MIT | `libs/crypto` (dependency) |
| `cmac` 0.7.2 | RustCrypto, crates.io | MIT OR Apache-2.0, taken under MIT | `libs/crypto` (dependency) |
| `dbl` 0.3.2 | RustCrypto, crates.io | MIT OR Apache-2.0, taken under MIT | `libs/crypto` (dependency of `cmac`) |
| MediaTek `mt76` (mt7921/mt7925 family) reference | Linux `drivers/net/wireless/mediatek/mt76`, commit not yet pinned | BSD-3-Clause-Clear | not yet taken: planned for `wifid` (driver half of the Wi-Fi plan) |
| OpenBSD `net80211` reference | OpenBSD `sys/net80211`, commit not yet pinned | ISC / BSD | not yet taken: planned as the reference for `libs/ieee80211` and `libs/eapol` |
| `linux-firmware` MediaTek blobs (`WIFI_RAM_CODE_MT7925_*`, `WIFI_MT7925_PATCH_*`) with `LICENCE.mediatek` | `linux-firmware.git`, commit not yet pinned | MediaTek redistribution licence (not open source) | not yet taken: planned at `/system/share/firmware/mediatek/` (WP6), fetched by a hash-pinned tool, never linked into a binary |

A row is updated, with the pinned commit and the file list, in the same
commit that brings the code or blob in. "Not yet taken" rows are promises of
where the material is expected to come from, not permission to copy it.
