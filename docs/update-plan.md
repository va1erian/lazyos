# Plan: updating an installed LazyOS

> **Status: draft proposal, revision 2 (2026-10-19).** Nothing here is built
> beyond what "What exists" says. Revision 2 consolidates revision 1 (#565,
> signed A/B partition slots for a whole-disk NVMe install) with the install
> that actually exists: **LazyOS beside Linux, behind the distro's GRUB**
> ([`installing-beside-linux.md`](installing-beside-linux.md), design in
> [`grub-install-plan.md`](grub-install-plan.md)). That install has exactly
> two LazyOS partitions and a static GRUB entry, so revision 1's spare slot
> partition, `lazyboot` partition and `state` volume do not exist there.
> Revision 2 keeps revision 1's bundle, signing, release pipeline, trial-boot
> and settings rules, and replaces partition slots with **release
> directories on the one system volume**. The same scheme serves a
> whole-disk install later (see "Other installs").
> Target: the Kaby Lake box ([`kabylake-box-plan.md`](kabylake-box-plan.md)),
> and any UEFI PC installed with `lazyos-install`.

The guide comes first (what a person types once this plan is built); the
design follows.

## Guide: install, then deploy an update

Steps marked **(today)** work now; the rest is this plan's target.

### 1. Prepare the Linux side (today)

On the PC's Linux, with Linux's own tools (details:
[`installing-beside-linux.md`](installing-beside-linux.md)):

- UEFI boot, Secure Boot off, GRUB working.
- Two empty GPT partitions on the ESP's disk, named `lazyos-system`
  (4 GiB or more; updates keep two or three releases there, see "Space") and
  `lazyos-home` (any size). `gdisk`, command `c`, sets the names.

### 2. Make the stick (today)

```bash
python tools/run_demo.py --desktop --installer --build-only
```

Write `target/lazyos-usb.img` to a USB stick ([`usb-stick.md`](usb-stick.md)).

### 3. Install (today, plus U0 and U2 for the update-ready layout)

Boot Linux, plug the stick in, copy the installer off the FAT partition:

```bash
cp /media/you/LAZYOS/install/lazyos-install /tmp/ && chmod +x /tmp/lazyos-install
```

```bash
sudo /tmp/lazyos-install --stick /media/you/LAZYOS --dry-run
```

```bash
sudo /tmp/lazyos-install --stick /media/you/LAZYOS --trial-boot
```

```bash
sudo reboot
```

`--trial-boot` boots LazyOS once; if it does not come up, the power button
gets you back to Linux. From U0, the install writes the stick's release as
`/releases/<release>/` on `lazyos-system`, the boot control files, and the
slot-aware loader on the ESP (see "Layout").

**Already installed with an older `lazyos-install`?** Run its `update` once
with a stick built after U0 and U2 (step 4c). It converts the volume in place
(a rename on the same volume, nothing copied, `/conf`, `/apps`, `/logs` and
`/home` untouched) and refreshes the GRUB entry. That is the only update that
needs Linux.

### 4. Deploy an update

Build a release bundle on the dev machine (U1):

```bash
python tools/update/build.py --key ~/.lazyos/update.sec
```

This writes `target/lazyos-<release>.lzu`, a signed bundle of the kernel,
`/system` (core services and core apps included) and `/docs/os`. Releases
tagged `v*` are built and signed by CI instead ("Release pipeline"), and the
key's public half is what the installed system trusts. Then pick one route:

**a. From LazyOS, a file (U3).** Copy the bundle anywhere LazyOS can read
(`~/Downloads`, a FAT stick at `/media/<label>`, a network drive), then in the
Terminal:

```bash
updatectl install ~/Downloads/lazyos-0.4.0.lzu
```

```bash
updatectl restart
```

`install` checks the signature, stages the release beside the running one,
checks every file's digest, and marks it to boot on trial. `restart` asks
`init` to shut down cleanly and tells GRUB to come back to LazyOS (not to the
Linux default). Settings > Updates does the same with buttons: *Install from
file...*, then *Restart now*.

**b. From LazyOS, the release feed (U4).** Settings > Updates > *Check now*
lists the newer release on the chosen channel (`stable` or `dev`) with its
notes; *Install*, then *Restart now*. From the Terminal:

```bash
updatectl check
```

```bash
updatectl install --feed
```

**c. From Linux, a stick (today in-place; U0+ staged).** Build a newer
stick, boot Linux, then:

```bash
sudo /tmp/lazyos-install update --stick /media/you/LAZYOS --trial-boot
```

From U0 this stages the stick's release exactly as `updatectl` does (same
library) instead of overwriting `/system`, so it gets the same fallback. It
is also the repair path when LazyOS no longer boots at all.

**d. Dev loop, over the network (U4).** With a `dbgd --dbgd-control` image or
any image whose `updated` trusts the dev key:

```bash
python tools/update/push.py --host <ip> --restart
```

The host serves the fresh bundle, `updated` fetches it, installs it and
restarts.

### 5. After the restart

- The release boots **on trial**. Once the desktop has come up and
  `healthd` reports `ok` for 60 s, `updated` marks it good and the toast says
  "Updated to 0.4.0".
- If it panics, hangs (power-cycle it) or never reaches a healthy desktop,
  three tries later the loader boots the previous release on its own, settings
  are restored to their pre-update copy, and the notice says why
  (`updatectl status`). If GRUB's own counter runs out first, the next boot is
  Linux, from where step 4c repairs it.

```bash
updatectl status
```

```bash
updatectl rollback
```

```bash
updatectl restart
```

`rollback` makes the previous release the next boot (also a trial, so a
rollback cannot strand the machine either). `updatectl list` shows the kept
releases (current, previous, base) and their state.

### 6. Applications

Core apps follow the release: they are in the bundle's `/system/packages`,
and `pkgd` upgrades `/apps` from it at the first boot of a new release (it
does this today) and, from U3, back down after a fallback or rollback. Apps you
installed yourself update without a restart (U5 for rollback):

```bash
pkgctl install ~/Downloads/foo-1.3.0.lzp
```

```bash
pkgctl rollback org.example.foo
```

A running instance keeps running the old version; the next launch uses the
new one. Double-clicking an `.lzp` in Files opens the Installer, which says
"Upgrade 1.2.0 to 1.3.0".

## Goal and scope

Move an installed machine from release N to N+1 (kernel, core services, core
apps, docs) from inside LazyOS, with one restart, and never leave it
unbootable: a power cut while staging, a bad download, a kernel that panics on
this hardware or a desktop that never comes up all end with release N running
again, on its own, with settings, apps and files intact. Update user-installed
apps without a restart, with a way back.

**In:** signed release bundles; staging from a file, a stick, the release feed
or a dev push; trial boots with automatic fallback; manual rollback;
`updatectl`, `updated`, a Settings page; core-app reconcile in both
directions; app rollback; the migration of existing installs.

**Out (for now):** updating the loader or the GRUB entry from inside LazyOS
(a rare `lazyos-install update` from Linux does that, see "The ESP"), Secure
Boot, firmware capsules, hot-swapping a running service for users (`dbgd`'s
`reload` stays a developer tool), Linux-side changes of any kind.

## What exists, what is missing

| Piece | Today | Needed |
|---|---|---|
| Install | `lazyos-install` (I0-I3): `lazyos-system` ext2 as `/` (programs, `/conf`, `/apps`, `/logs`), `lazyos-home` as `/home`, `\EFI\lazyos\{BOOTX64.EFI, boot.json, kernel-x86_64, ramdisk}` on the ESP, a static `40_lazyos` GRUB entry that chainloads the loader | `/releases/` on the system volume, the boot control files, the slot-aware loader, a GRUB snippet that can return to LazyOS (U0, U2) |
| Update | `lazyos-install update` from Linux rewrites `/system` and `/docs/os` in place from `/system/.image-manifest` (`build_support/os_update.rs`, shared), kernel last; a power cut can mix releases | staging into a new directory, an atomic switch, fallback |
| Kernel location | on the ESP (FAT32): LazyOS cannot read or safely write it (the kernel has no FAT32; `libs/fatvol` could write it from user space, but a torn FAT write would damage Linux's ESP too) | the kernel inside each release on ext2, read by the loader |
| Loader | vendored `bootloader` 0.11.17 UEFI stage, patched to read its own directory (`vendor/bootloader/uefi/src/boot_dir.rs`) | read ext2 through UEFI `BlockIO` with `libs/ext2fs` (already `no_std`), choose a release, count tries |
| Boot target after a restart | GRUB's default is usually Linux and the menu hidden; LazyOS cannot run `grub-reboot` (grubenv is on Linux's ext4) | a GRUB env block on *our* ext2 volume that the snippet reads (U2) |
| VFS | mounts take any `Arc<dyn Filesystem>`; one filesystem may be mounted twice (`/tmp`, `/transient`); `ro`/`nosuid` flags exist | a subtree mount (a directory of a mounted volume as `/system`) |
| Packages | `pkgd` installs beside, switches, deletes the old directory; provisioning upgrades core apps up only (`pkgstore::provision`) | down after fallback; keep one previous version for rollback |
| Integrity | SHA-256 (`libs/crypto`); `ed25519-dalek` in `nettls-crypto`; netboot's Ed25519 manifest signing (`tools/netboot/`) | the same signature over the bundle manifest |
| Health | `healthd` summary, `SHELL:DESKTOP:PASS` | a "this boot is good" decision |
| Panic | `panic_screen` halts | reboot after 10 s on a trial boot |

## Approaches considered

1. **In place, as today.** Simple; a power cut mixes releases and there is
   nothing to fall back to. Kept only as the pre-U0 path.
2. **A/B partitions (revision 1).** The robust classic, but the GRUB install
   has no spare partitions, the person made the partitions with Linux tools,
   and asking for three more (slot B, control, state) to update is a big
   ask. It also needs raw partition writes from user space.
3. **Release directories on the system volume. Chosen.** Each release is a
   complete, read-only tree (`/releases/<id>/`) beside the others on the one
   ext2 volume; state stays where it is; the loader picks which tree's kernel
   to load and the kernel mounts that tree's `system/` at `/system`.
   Unchanged files are hard links to the previous release, so a release costs
   only what changed. A torn update damages only a directory nobody boots.
   The weakness against A/B: one filesystem, so a corrupted volume takes all
   releases with it. That is the risk the ESP's rescue kernel and the stick
   (step 4c) cover.

## Key decisions

1. **Layout of `lazyos-system`.**

   ```
   /releases/
     0.3.0/                 the running release (read-only by policy and mount)
       kernel-x86_64
       system/              bin lib share packages etc docs  -> mounted at /system
       RELEASE              the bundle's manifest, signature beside it
     0.4.0.partial/         being staged; deleted on sight by updated
     bootctl                8 KiB, preallocated, written only in place
     grubenv                1024 B GRUB environment block, written only in place
   /apps /conf /logs /accounts /home(mount point) /lost+found ...
   ```

   `/docs/os` moves into the release as `/system/docs` (one `libs/fhs`
   constant; `/docs/apps` stays). `/system/.image-manifest` is replaced by
   `RELEASE`. Everything outside `/releases` is state and no update touches
   it.
2. **`/system` is a read-only subtree mount.** The kernel mounts
   `/releases/<booted>/system` at `/system` with `ro,nosuid` (a small
   `SubtreeFs` adapter over the root ext2 instance: same inode cache key, a
   root inode other than 2). Today "only the build writes `/system`" is a
   policy; after U0 it is a mount flag. `/system/etc` holds seeds the running
   system also writes to (`os_update.rs` exempts it): U0 audits every writer
   and moves the mutable files under `/conf`, leaving `/system/etc` as
   per-release defaults.
3. **The kernel lives in the release; the ESP holds only stable files.** The
   loader reads `/releases/<id>/kernel-x86_64` from ext2. The ESP keeps the
   loader, `boot.json`, the config ramdisk (`root=UUID=`, `home=UUID=`, limits:
   nothing a release changes) and one **rescue kernel**, the kernel of the
   release `lazyos-install` installed (the *base* release, pinned in
   `/releases` so it always has its tree). LazyOS never writes the ESP;
   `lazyos-install` (from Linux) refreshes all four together.
4. **The loader chooses the release.** `\EFI\lazyos\BOOTX64.EFI` gains, before
   loading anything: find the system partition by the GUID in
   `\EFI\lazyos\system.id` (written by the installer) through `BlockIO`, open
   it read-only with `libs/ext2fs`, read `bootctl`, apply the rules (decision
   5), write the updated control record **into the file's existing blocks**
   (raw `BlockIO` write of the block the inode already maps: no allocation,
   no metadata change, safe on an unclean volume), then load that release's
   kernel. Anything unreadable (no `system.id`, an ext2 error, both `bootctl`
   copies bad, a missing kernel) loads the ESP's rescue kernel, which boots
   its pinned base release. Every decision is printed on the loader's screen
   and recorded in `bootctl` for the kernel to log (`UPDATE:BOOT:<id>
   TRY:<n> REASON:<..>`).
5. **Boot control (`libs/bootctl`, `no_std`, shared by loader, kernel,
   `updated` and `lazyos-install`).** Two 512-byte records in `bootctl` (sector
   0 and sector 8, one per 4 KiB block), each: magic, version, sequence, CRC-32,
   and up to four entries `{release id, state, tries_left}` where state is
   `good`, `trial` or `bad`, plus `next` (the release to boot), `booted`
   (what the loader chose this time) and `fell_back` (and from what). A
   writer writes the copy with the older sequence, then the other; a reader
   takes the newer valid one. Rules, applied by the loader:
   - `next` is `good`: boot it;
   - `next` is `trial` with `tries_left > 0`: decrement, boot it;
   - otherwise mark it `bad`, set `next` to the newest `good` entry, set
     `fell_back`, boot that;
   - no `good` entry: the rescue kernel.
   The kernel reads `booted` to know which tree to mount; a kernel that finds
   `booted` is not its own release id (a stale record) panics with that text
   instead of mounting the wrong tree.
6. **Coming back to LazyOS after a restart (GRUB).** GRUB's default stays
   the person's choice (usually Linux), so a plain reboot from LazyOS would
   land in Linux and a trial could never retry. The installer's snippet gains
   a few lines outside the `menuentry` (GRUB evaluates them when it reads
   `grub.cfg`, after `00_header` has set `default`):

   ```
   search --no-floppy --fs-uuid --set=lzsys @SYSTEM_UUID@
   if [ -n "$lzsys" ]; then
     load_env -f ($lzsys)/releases/grubenv lazyos_boots
     if   [ "$lazyos_boots" = 3 ]; then set lazyos_boots=2; set lz=1
     elif [ "$lazyos_boots" = 2 ]; then set lazyos_boots=1; set lz=1
     elif [ "$lazyos_boots" = 1 ]; then set lazyos_boots=0; set lz=1
     fi
     if [ "$lz" = 1 ]; then
       save_env -f ($lzsys)/releases/grubenv lazyos_boots
       set default=lazyos; set timeout=0
     fi
   fi
   ```

   GRUB script has no arithmetic, hence the ladder (Fedora's boot counting
   does the same). `save_env` rewrites an existing env block in place, which
   GRUB allows on plain ext2. `updatectl restart` (and LazyOS's own *Restart*,
   which then means "restart LazyOS") writes `lazyos_boots=3` before asking
   `init` to reboot; `updated` writes `0` once the boot is marked good or a
   fallback release came up. So a restart returns to LazyOS, a failing trial
   gets up to three boots (the loader's counter falls back inside them), and
   the worst case ends in Linux. *Power off* never sets it, so a cold boot is
   the person's default as before.
7. **"Good" means the session came up** (revision 1, unchanged). `updated`
   marks the booted release `good` once `healthd`'s summary is `ok` and the
   session has run 60 s. A trial boot reboots 10 s after a panic instead of
   halting (the panic screen shows the countdown); a hang needs the power
   button until a watchdog driver exists for the box. Three tries.
8. **A signed bundle, trust carried by the running release.** An `.lzu` is a
   zip under `lazypkg`'s container rules (no zip64, stored or deflate, path
   checks) holding `manifest.toml`, `manifest.sig` (Ed25519 over the
   manifest's bytes, the format netboot uses) and the tree: `kernel-x86_64`
   and `system/...`. The manifest (`deny_unknown_fields`) names the release,
   the channel, the minimum release it may be installed over, the layout
   version, and every file with mode, owner, size and SHA-256. Trusted keys
   are `/system/etc/update/keys/*.pub` of the **running** release, so a
   release can rotate them for the next. The signature is checked before
   anything else is read; a release older than the running one is refused
   unless `--allow-downgrade`.
9. **Staging is copy-or-link, then one rename.** `updated` creates
   `/releases/<id>.partial/`; for each manifest entry it hard-links the file of
   the newest kept release with the same digest, or extracts it from the
   bundle; then barriers, reads every new file back against its digest, renames
   the directory to `/releases/<id>`, and only then writes `bootctl` (`next =
   id`, `trial`, 3 tries). A power cut before the rename leaves a `.partial`
   that the next `updated` start deletes; after it, an unbooted release that
   `updatectl list` shows. Files are never modified once staged (the mount is
   read-only and `updated` only creates), which is what makes the hard links
   safe. An unclean volume (the kernel has no fsck) refuses staging with the
   remedy (`lazyos-install check`, or 4c), since a leaked-block volume can
   still be booted but should not be filled.
10. **`updated`: one service, one directory.** It runs as `_update` with no
    capability. The kernel's file policy lets only `_update` create or delete
    under `/releases` (and write in place to `bootctl` and `grubenv`), and
    nothing writes under a mounted `/system`. It serves `os.lazy.update.v1`
    (`idl/update.midl`: `Status`, `Check`, `Install(file | feed)`,
    `Rollback`, `List`, `Restart`; `MarkGood` only from its own health watch)
    to `updatectl` and Settings. Installing, rolling back and restarting go
    through `elevd` (`update.install`, an administrator's prompt) like every
    privileged change. It logs to `/logs/update.log` and traces on a new
    `UPDATE` channel.
11. **Settings survive a way back** (revision 1, unchanged). Before the first
    boot of N+1, `updated` snapshots `/conf` to `/conf/.pre-<id>/`. A release
    may add keys and must keep reading the previous release's keys. When the
    loader fell back or a rollback ran, `init` restores the snapshot before it
    starts `confd`, so release N never reads N+1's format. Snapshots are kept
    for the releases still on disk.
12. **Core apps go both ways.** Provisioning today upgrades a core row only
    upward and keeps a newer version "the user installed". U3 records who
    installed a row (`installed_by = provision | user`) so a row provisioning
    put there is brought to the shipped version in both directions, while a
    version the person installed stays.
13. **Space.** `updated` keeps the running release, the previous `good` one
    and the pinned base, and deletes others (oldest first) only after a new
    release is marked good. Before staging it sums the sizes of the files it
    cannot link and refuses with the numbers when the volume lacks them plus a
    64 MiB margin. A 4 GiB partition holds three desktop releases with room to
    spare; a WebKit or SRB2Kart image needs the larger partition its build
    already asks for.

## Architecture

```
 release CI (or a dev host)
   build ─► tree (kernel + system/) ─► manifest.toml (digests) ─► sign ─► lazyos-<id>.lzu
           └─► feed.json per channel on GitHub Releases

 updated (LazyOS)                      lazyos-install update (Linux)
   file │ feed │ push                    stick
     └──────────────┬─────────────────────┘
          libs/lzupdate: verify signature ─► stage /releases/<id>.partial (link or extract)
          ─► read back ─► rename ─► bootctl: next=<id> trial tries=3 ─► /conf snapshot
   updatectl restart ─► grubenv lazyos_boots=3 ─► init.Shutdown(reboot)

 firmware ─► GRUB: grubenv says LazyOS ─► chainloader \EFI\lazyos\BOOTX64.EFI
 loader   ─► ext2 read of lazyos-system ─► bootctl rules ─► tries-1 written in place
          ─► /releases/<id>/kernel-x86_64        (or the ESP rescue kernel)
 kernel   ─► / = lazyos-system, /system = /releases/<id>/system (ro), /home
 init     ─► conf restore if fell_back ─► pkgd reconcile (up or down) ─► desktop
 updated  ─► healthd ok for 60 s ─► bootctl good, grubenv 0, prune old releases
```

## The ESP and the loader's own updates

LazyOS never writes the ESP: it is Linux's, FAT32 is not crash-safe, and a
torn write there could stop Linux booting too. The four ESP files change only
when `lazyos-install update` runs from Linux, which also refreshes the base
release and the GRUB snippet. A bundle whose manifest names a newer loader
than the running one (`requires_loader`) is refused by `updated` with that
message; the release notes say so. Loader changes are expected to be rare
once U2 has landed.

## Other installs

- **The USB stick** runs from its RAM root and is not updated; a new stick is
  its update.
- **A whole-disk install** ([`nvme-install-plan.md`](nvme-install-plan.md))
  uses this same scheme on its system volume and needs neither revision 1's
  second slot partition nor its `lazyboot` and `state` partitions; that plan's
  layout table should shrink accordingly. With no GRUB in front, the loader is
  the firmware's boot entry and decision 6's grubenv is simply unused.

## Release pipeline

Unchanged from revision 1 (decided 2026-10-04): `.github/workflows/release.yml`
builds every `v*` tag. From U1 it also builds the `.lzu`; a separate `sign`
job, in the `release` GitHub environment (tag refs `v*` only, maintainer
approval), downloads the bundle, signs `manifest.toml`, uploads
`manifest.sig`, and checks out no code. `v*` tags are protected. The public key
is committed as `/system/etc/update/keys/release.pub`; rotation is one release
signed by the old key carrying the new public key. An offline copy of the
private key is the recovery path; losing it means reinstalling from a stick.
A developer key (`tools/update/keygen.py`) is trusted only by images built with
`LAZYOS_UPDATE_DEV_KEY=<pub>`, never by release images.

## Phases

Each is shippable. Kernel work ships correctness and stress tests under
`kernel/src/tests/` and passes `python tools/test/run.py --accel none`;
libraries ship `cargo test` and seeded fuzz entries; `.rs`/`.py` files stay
under 500 lines. The harness is `tools/update/run.py`, built on
`tools/install/run.py` (`--via grub` in CI, `--via shell` anywhere).

| Phase | Deliverable | Evidence |
|---|---|---|
| **U0** Layout and migration | `/releases/<id>/` layout written by the image build and by `lazyos-install` (fresh install and conversion of an existing volume by rename); `SubtreeFs` and the `ro,nosuid` `/system` mount; the kernel picks its tree from `bootctl.booted` or, without a record, its built-in release id; `/docs/os` to `/system/docs`; mutable `/system/etc` files moved to `/conf`; `python tools/fhs/check_literals.py` green | `fs` suite: subtree mount reads, `EROFS` on every write path, stale `booted` refused, a soak of mount/unmount; `cargo test -p lazyos-install -p build-support-tests`: conversion keeps `/conf`, `/apps`, `/logs` byte-identical; harness: install an old-layout volume, convert, boot, the desktop and a confd key are there |
| **U1** Bundles and staging | `libs/lzupdate` (manifest, container via `lazypkg`'s reader, Ed25519 verify, staging with links, `.partial` reclaim, space check), shared by the build, `lazyos-install` and `updated`; `tools/update/build.py`, `keygen.py`; `LAZYOS_UPDATE_KEY` makes `cargo build` write the `.lzu`; the `sign` job; `lazyos-install update` stages instead of overwriting | `cargo test -p lzupdate`: wrong key, a flipped byte per file, unknown manifest field, downgrade, path escape, zip64, oversize; a power cut at every write of staging (ext2fs crash prefixes) leaves the running release bootable and the next start reclaims; fuzz on manifest and container |
| **U2** Loader, control, GRUB | `libs/bootctl`; the loader's ext2 read, rules and in-place write (`vendor/bootloader/uefi`); `system.id`; the rescue path; the grubenv snippet in `40_lazyos.in` and `custom.cfg`, written by `lazyos-install`; the trial-boot panic reboot | `cargo test -p bootctl`: torn copies, CRC, sequence wrap, every rule; harness under OVMF through real GRUB: a release whose kernel panics is tried three times then the previous one boots with `UPDATE:FALLBACK`; `lazyos_boots` counts down to Linux when every release fails; a wiped `bootctl` boots the rescue kernel; Linux's entry and files byte-identical |
| **U3** `updated`, `updatectl`, Settings | `idl/update.midl`, the service and its file policy, `elevd` action `update.install`, `/conf` snapshot and restore in `init`, `pkgd` two-way core reconcile, Settings > Updates (file install, status, rollback, restart), `UPDATE` trace channel, launcher and `run_demo.py --update-dev-key` | harness: install N+1 from `/home`, restart, `UPDATE:GOOD`, `/system` is N+1, core apps upgraded, settings kept; kill QEMU mid-staging, N still boots and `updatectl status` explains; rollback returns to N with core apps and `/conf` restored; account attack harness: the session cannot write `/releases` or call `Install` without the prompt; screenshot of the Settings page read |
| **U4** Feed and push | the feed client (rustls, system CA bundle), channels, a daily check and a notification, `tools/update/push.py` and the `dbgd` path for the dev loop | a local HTTPS server under the test CA (as `tools/net/tls_run.py`): found, downloaded, installed, booted; truncated download, wrong digest, stale feed refused and explained |
| **U5** Apps | `pkgd` keeps the previous version directory of an upgraded user app, `pkgctl rollback`, the Installer's "Upgrade x to y" text, an optional per-app feed in the manifest | `cargo test -p pkgstore`: upgrade, rollback, a power cut at every step; session: install 1.2, upgrade to 1.3 while it runs, relaunch, roll back |
| **Later** | file-level delta bundles (only the files the target release lacks, made possible by U1's links), background download, an install-at-a-time setting, a watchdog on the box, a Multiboot2 GRUB entry that loads the kernel from ext2 directly (grub-install-plan "Later") | as each lands |

**Order on the box:** U0 and U2 together make the first `lazyos-install
update` from Linux the last mandatory one; U1 and U3 give updates from a file
with fallback; U4 gives one click; U5 is independent and can go first.

## Risks and open questions

1. **One filesystem.** A corrupted `lazyos-system` hurts every release. The
   rescue kernel, `e2fsck` from Linux and the stick are the answers; the
   journal stays off for the reasons grub-install-plan gives.
2. **The loader writes the volume.** Only whole sectors the `bootctl` inode
   already owns, found by a read-only walk and checked against the inode's
   size before writing; still, a loader bug here is the worst kind. `bootctl`
   tests run against the same `BlockIO` shim the loader uses, under the ext2
   checker.
3. **GRUB's `save_env` on our partition.** Upstream allows it on plain ext2
   and refuses on LVM, RAID, btrfs and encrypted volumes, none of which this
   install uses; it is to confirm on Ubuntu's GRUB 2.14 in I3's `--via grub`
   run and on the box. A distro that ignores the snippet (BLS-only) still boots
   LazyOS from the menu; `updatectl restart` then says the restart will need
   the menu.
4. **`/system/etc` writers.** The audit in U0 may find a service that relies
   on writing there; each one moves to `/conf` or the plan needs a writable
   overlay for that file.
5. **Who may update.** This plan says an administrator through `elevd`. A
   single-user box may want "any session user"; that is a policy line in
   `elevpolicy`, not a design change.
6. **Release ids.** Semantic versions from the root `Cargo.toml`, plus the
   commit for dev builds (`0.4.0-dev.<sha8>`), compared by `lazypkg`'s
   version rules; two different trees with one id are refused.
