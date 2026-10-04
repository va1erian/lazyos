# Plan: updating an installed LazyOS

> **Status: draft proposal, revision 1 (2026-10-04).** Nothing here is built.
> Target machine: an Intel N150 mini PC running LazyOS full time from its
> internal NVMe disk. Builds on [`real-pc-boot-plan.md`](real-pc-boot-plan.md)
> and [`usb-stick.md`](usb-stick.md) (how a real PC boots today),
> [`filesystem-plan.md`](filesystem-plan.md) (the `/system` versus `/apps`,
> `/conf`, `/logs` split), [`packages.md`](packages.md) (`.lzp`, `pkgd`) and
> [`tls-plan.md`](tls-plan.md) (HTTPS clients). The S9 "release images" line
> of [`platform-plan.md`](platform-plan.md) is the release side of this plan.
> Two sibling efforts are hard dependencies and are named where they bite: the
> **NVMe install** (a kernel NVMe driver and the on-disk layout) and the
> **N150 driver package** (a real NIC, a watchdog).

## Goal and scope

Move an installed machine from release N to release N+1 without a USB stick
and without ever leaving it unbootable: a power cut, a bad download, a kernel
that panics on this hardware or a desktop that never comes up must all end
with the machine running release N again, on its own, with the user's
settings, apps and files intact.

**In (v1):** the OS itself (kernel, `/boot/lazyos.cfg`, `/system`,
`/docs/os`), shipped as a signed bundle; installing it from a file (a USB
stick, `/home`, a download) and from an HTTPS release feed; automatic
fallback when a new release fails to boot; a manual rollback; `updatectl`
and a Settings page.

**Out (v1):** delta updates (full images are about 40 MiB compressed),
updating the boot shim itself (rare, done by reinstalling), firmware
(BIOS/UEFI capsule) updates, Secure Boot, and app updates, which `pkgd`
already owns (core apps follow the image; see "After the switch").

## What exists, what is missing

| Piece | State today | Needed |
|---|---|---|
| Separation of OS and state | `/system`, `/docs/os` and `/boot` are written only by image updates; `/apps`, `/conf`, `/logs` survive them; `/home` is its own volume (`filesystem-plan.md`) | the same split on **separate volumes**, so the OS can be replaced as a whole while state stays put |
| Image update | the host build rewrites `/boot`, `/system`, `/docs/os` of `target/lazyos.img` offline, keeping state; `pkgd` upgrades core packages at the next boot after an image update (`libs/pkgstore/src/provision.rs`) | the same, done by the running system, atomically |
| Boot path | `bootloader` 0.11.17 UEFI stage (`\EFI\BOOT\BOOTX64.EFI`) loads `kernel-x86_64`, `ramdisk` and `boot.json` by fixed names from **its own partition**; no menu, no fallback, no UEFI runtime services handed to the kernel | a choice between two installed releases, with boot counting |
| Partitions | MBR only; `kernel/src/block/partition.rs` refuses GPT | GPT, planned by the NVMe install (#564, N2); this plan adds two partitions to its layout |
| Disk writes | ATA read-only; virtio-blk read/write; USB mass storage through `usbd` | an NVMe driver with writes (NVMe install effort) |
| FAT | read-only FAT12/16 in the kernel; FAT32 refused (#248) | nothing for the OS slots (written as whole images); FAT32 read for the "update from any USB stick" source |
| Integrity | SHA-256 (`libs/crypto`); core package index pins digests; `nettls-crypto` carries `ed25519-dalek` | a signature check on the bundle manifest |
| Download | `fetch`/`curl`/`wget` over rustls with the system CA bundle (T0-T3); `netd` over virtio-net only | a NIC driver for the N150 box (driver package effort) |
| Health | `healthd` publishes `system/health/summary`; the desktop logs `SHELL:DESKTOP:PASS` | a "this boot is good" decision that marks the slot |
| Panic | `panic_screen` draws the panic and halts | reboot after a delay when a trial boot panics, so fallback needs no one at the keyboard |

## Approaches considered

1. **Replace files in place** (what the host build does offline). Simple, and
   wrong for a running machine: a power cut halfway through `/system` leaves a
   mix of N and N+1 that may not boot, and there is nothing to fall back to.
2. **Package-manager style** (every OS component a package, upgraded one by
   one). Same atomicity problem, plus dependency resolution LazyOS does not
   need: the OS is built and tested as one image.
3. **Image-based A/B slots** (ChromeOS, Android, RAUC, Mender). Two copies of
   the OS; the running one is never touched; the update is written whole to
   the other, verified, and booted on trial; failure falls back. Costs disk
   space (two slots of about 2 GiB on a disk of 256 GiB or more) and a small
   boot shim. **Chosen.**

## Key decisions

1. **A/B slots, each one ext2 image.** The NVMe install
   ([`nvme-install-plan.md`](nvme-install-plan.md), #564) already reserves two
   4 GiB root slots. A slot here holds the whole OS and nothing else: the
   kernel at `boot/kernel-x86_64`, then `bin/`, `etc/`, `share/`,
   `packages/` and `docs/` (what `/system` and `/docs/os` hold today). The
   build already writes such an ext2 volume with `libs/ext2fs`; the updater
   writes it block for block to the inactive slot and reads it back against
   the manifest's digest. The kernel never needs to write FAT or replace ext2
   files, and an interrupted write damages only the slot that is not running.
2. **The slot is read-only; state moves to its own volume.** `/` is a
   persistent ext2 **state** volume holding `/apps`, `/conf`, `/logs`; the
   active slot is mounted at `/system` `ro,nosuid` (`/docs/os` becomes
   `/system/docs`, one `libs/fhs` constant); `/home` stays its own volume.
   This is the F-series split taken one step further, and it makes "the OS
   cannot be modified at runtime" true by construction. With in-place
   updates the state could have stayed on the root volume; with alternating
   slots it cannot, because slot B would start without A's settings and apps.
3. **A boot shim chooses the slot.** The ESP's `BOOTX64.EFI` is a fork of the
   `bootloader` 0.11.17 UEFI stage (one file, `uefi/src/main.rs`) that,
   before loading anything, reads the **boot control block** from a 1 MiB raw
   `lazyboot` partition through UEFI `BlockIO`, picks a slot, writes the
   decremented try counter back (firmware storage is writable before
   `ExitBootServices`), and reads `boot/kernel-x86_64` out of that slot's
   ext2 volume with `libs/ext2fs` (already `no_std`, read path only) over
   `BlockIO`, instead of loading `kernel-x86_64` from its own partition.
   Everything after that is unchanged `bootloader` code. With no valid
   control block it loads the ESP's own `kernel-x86_64` exactly as #564's
   install does today, so a machine installed before updates exist still
   boots, and the ESP kernel stays the last-resort path. UEFI
   `BootNext`/`BootOrder` would avoid the fork but needs UEFI runtime
   services in the kernel, which `bootloader` 0.11 does not pass on, and
   vendor firmware honours `BootNext` unevenly.
4. **Boot control block** (`libs/bootctl`, pure `no_std`, shared by the shim,
   the kernel and the host tools): magic, version, `active` slot, and per
   slot `{partition GUID, release, tries_left, successful}`, CRC-32 over the
   whole; stored twice (LBA 0 and LBA 1 of `lazyboot`) with a sequence
   number, written one copy at a time, the newer valid copy wins. Rules,
   applied by the shim:
   - the active slot with `successful` boots;
   - the active slot without `successful` and `tries_left > 0` boots after
     `tries_left -= 1` is written;
   - otherwise the other slot becomes active, if it is `successful`;
   - neither bootable: the shim says so on screen and loads the ESP kernel
     (and any USB stick carrying LazyOS still works as a rescue medium).
   The kernel reads the block at boot to learn which slot it runs from,
   mounts that partition at `/system` (overriding a `system=` line in the
   ESP's `lazyos.cfg`, which keeps `root=` for the state volume, `home=` and
   the limits) and logs `UPDATE:SLOT:<a|b> TRY:<n>`.
5. **"Good" means the session came up.** `updated` marks the running slot
   `successful` (and stops counting) once `healthd`'s summary is `ok` and the
   session has run for 60 s; until then a panic or a hang costs one try. Trial
   boots reboot on panic after 10 s instead of halting, and the iTCO watchdog
   (driver package) covers hangs. Three tries by default.
6. **A signed bundle, trust carried by the image.** A `.lzu` file is a zip
   (the `lazypkg` container rules: no zip64, deflate or stored, path checks)
   holding `manifest.toml` and `system.img` (the slot volume, kernel
   included). The manifest names the release, the channel, the minimum
   release it may be installed over, the hardware profile, and the image's
   size and SHA-256;
   `manifest.sig` is an Ed25519 signature of the manifest's bytes. Trusted
   keys live in `/system/etc/update/keys/`, so a release can rotate them for
   the next one. The updater checks the signature before reading anything
   else and refuses a release older than the running one unless asked
   (`updatectl install --allow-downgrade`).
7. **`updated` is a service with one narrow capability.** It holds write
   access to exactly the inactive slot's partition and the `lazyboot`
   partition, granted by the kernel by partition GUID; it serves
   `os.lazy.update.v1` (check, download, install, status, mark-good,
   rollback) to `updatectl` and the Settings app, under policy like every
   other privileged action. Nothing else can write a slot.

## Disk layout (NVMe, GPT)

The layout is owned by [`nvme-install-plan.md`](nvme-install-plan.md)
("Disk layout"), which reserves what this plan needs from day one: slot A
(`lazyos-a`, carrying `boot/kernel-x86_64`), an empty slot B, the 1 MiB raw
`lazyboot` partition, the 8 GiB `state` volume, and home last so it can grow.
The ESP keeps a fallback kernel and `lazyos.cfg`. Until U0 and U2 land, the
install boots the ESP kernel with slot A as its whole OS volume, and the
partitions for slot B, `lazyboot` and `state` stay empty, so nothing is
repartitioned later. The table lives there; this plan decides what partitions 2 to 5 hold, and any change to them is made here first and then mirrored there.

If updates have to come before the slot split, the interim is reinstalling
from the stick; see "Sources".

## Architecture

```
 release CI (or a dev host)
   build ─► system.img (slot volume) ─► manifest.toml ─► sign (Ed25519) ─► lazyos-<rel>.lzu
            └─► feed.json on GitHub Releases (channel ─► latest release, URL, size)

 updated (on the machine)
   source: feed over HTTPS │ file on /home │ USB stick ─► verify manifest.sig
   ─► stream system.img into the inactive slot's partition
   ─► read back, compare SHA-256 ─► bootctl: other slot active, successful=0, tries=3
   ─► ask to restart

 boot shim (ESP)        reads lazyboot ─► picks slot ─► tries-1 ─► kernel from lazyos-<x>/boot
 kernel                 reads lazyboot ─► / = state, /system = lazyos-<x> (ro), /boot = ESP (ro)
 init, healthd          session up ─► health ok for 60 s ─► updated: mark successful
 pkgd                   core packages differ from /apps ─► upgrades them (exists)
 failure                panic (reboot in 10 s) │ hang (watchdog) ─► tries run out ─► previous slot
```

### After the switch

- **Core apps** follow the image already: `pkgd` compares
  `/system/packages/index` with `/apps` at boot and upgrades.
- **Settings** in `/conf` must stay readable by release N after N+1 has run,
  because fallback and rollback keep the state volume. Rule: a release may add
  confd keys and must keep reading old ones for one release; a migration that
  rewrites keys first copies `/conf` to `/conf/.pre-<release>/`, and a
  rollback restores it. `updated` refuses to roll back past a release that
  changed the store without such a copy.
- **Logs** record every step in `/logs/update.log` (source, release, digests,
  slot, verdict), and `updatectl status` shows the last attempt and why it
  failed, in the "denials are explained" spirit of the security model.

## Sources

| Source | Needs | Notes |
|---|---|---|
| HTTPS feed | N150 NIC driver (driver package), `netd`, rustls (built) | `feed.json` per channel on GitHub Releases of `va1erian/lazyos`; checked at login and daily, never installed without the user's click in v1 |
| File | nothing new | `updatectl install /home/user/lazyos-0.3.lzu`; also the path the Settings page uses after a browser or `fetch` download |
| USB stick | `usbd` storage (built) and FAT32 read (#248), or an ext2-formatted stick | most sticks come FAT32; until #248 lands, `tools/update/write_stick.py` writes an ext2 stick holding the bundle |
| Dev host push | NIC, `netd` | `tools/update/push.py --host <ip>`: the build host serves the fresh bundle and asks `updated` to fetch it; the fast loop for testing on the real machine |
| Reinstall | nothing new | boot the USB stick image and run the installer (NVMe install effort) over the active slot, keeping `state` and `lazyhome`: the update path before `updated` exists, and the repair path after |

## Phases

Each is independently shippable. Kernel-facing phases ship correctness and
stress tests under `kernel/src/tests/` and pass `python tools/test/run.py
--accel none`; host libraries ship `cargo test` and fuzz seeds; every phase
adds a QEMU run to `tools/update/run.py` (OVMF, an NVMe or virtio disk laid
out as above), because a broken updater is found in CI, not on the mini PC.

| Phase | Deliverable | Tests and evidence |
|---|---|---|
| **U0** Layout | With the NVMe install (#564): its GPT reader and layout, state moved from slot A to the `state` partition, the build writing the slot volume and the state volume separately, `system=` in `lazyos.cfg`, `/` on the state volume and `/system` mounted read-only from the slot, `/docs/os` moved to `/system/docs`; reinstall-over-slot keeping state and home | `fs` suite: `system=` mounts, writes to `/system` fail with `EROFS`; harness: install, write a file to `/home` and a confd key, reinstall a newer build, both survive |
| **U1** Bundles | `libs/lzupdate` (manifest schema with `deny_unknown_fields`, container via `lazypkg`'s reader, Ed25519 verify), `tools/update/build.py` (images from `target/`, manifest, sign with a key file), `cargo build` writes `target/lazyos-<rel>.lzu` when `LAZYOS_UPDATE_KEY` is set; CI attaches unsigned bundles to every run and signed ones to tags | `cargo test -p lzupdate`: wrong key, flipped byte in each image, manifest with an unknown field, downgrade, wrong hardware profile, oversize entry, zip64; fuzz entry on the manifest and container; `test_build.py` cross-checks host and guest rules |
| **U2** Boot shim and control block | `libs/bootctl` (host-tested), the forked UEFI stage as `build_support/shim/` producing `BOOTX64.EFI` (control block, ext2 read of the slot's kernel, fallback to the ESP kernel), kernel reads the block, `UPDATE:SLOT` line, trial-boot panic reboots after 10 s | `cargo test -p bootctl`: torn writes of either copy, CRC errors, sequence wrap, every rule above; harness under OVMF: boot A; mark B pending with a kernel that panics, the machine ends on A after three tries with `UPDATE:FALLBACK` logged; corrupt `lazyboot` entirely, the ESP kernel still boots |
| **U3** `updated` and `updatectl` | the service, the `os.lazy.update.v1` IDL, the kernel partition grant, install from a file, mark-good from health, rollback, `/logs/update.log` | harness: install bundle N+1 from `/home`, reboot, `UPDATE:GOOD b`, `/system` is N+1, `/apps` core packages upgraded, settings kept; power off QEMU mid-write, reboot, still on N and `updatectl status` explains; `updatectl rollback` returns to N; security suite: an unlabelled app cannot reach `os.lazy.update.v1` and no process but `updated` can open a slot partition |
| **U4** Network and UI | feed client in `updated` (rustls, the system CA bundle, the feed's TLS pinned to GitHub's hosts), channels `stable` and `dev`, daily check, a Settings "Updates" page (check, release notes, install, restart, rollback, last result), `tools/update/push.py` | harness with a local HTTPS server under the test CA (as `tools/net/tls_run.py`): update found, downloaded, installed, booted; truncated download, wrong digest, expired feed are refused and explained; screenshot of the Settings page read |
| **U5** Later | delta bundles (per-block, against the inactive slot's known release), background download, automatic install at a chosen hour, conf snapshots generalised, slot-aware recovery menu in the shim | as each lands |

## On the N150 box

Order of what has to exist before the machine can update itself:

1. NVMe install (#564) with the U0 layout: reinstalling
   from the stick is the update path at this point.
2. U1 + U2 + U3: updates from a file on `/home` or an ext2 stick, with
   fallback. No NIC needed.
3. The NIC driver from the driver package, then U4: one click in Settings.
4. The iTCO watchdog from the driver package: fallback from a hang without
   anyone pressing the power button.

## Open questions

- **Who signs releases?** A key held only on the developer's machine (sign
  locally, upload by hand) or a GitHub Actions secret (every tag signed by
  CI). The second is convenient and makes the CI account the root of trust.
- **Where does state live?** This plan adds a `state` partition. The
  alternative is keeping `/apps`, `/conf`, `/logs` on the home volume, which
  saves a partition but mixes system state with user files on a volume users
  are told is theirs. Proposed: a separate partition.
- **Does the boot shim ever update?** v1 says no: changing `BOOTX64.EFI` is a
  reinstall. If it must, write `BOOTX64.EFI.new` beside it and let the shim
  of N+1, once marked good, rename it through UEFI file protocol on the next
  boot.
