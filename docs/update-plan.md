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

Steps marked **(today)** work now. Everything else is this plan's target, and
the phase that brings it is named: `updatectl`, `tools/update/*.py`,
`lazyos-install check` and `pkgctl rollback` do not exist yet.

### 1. Prepare the Linux side (today)

On the PC's Linux, with Linux's own tools (details:
[`installing-beside-linux.md`](installing-beside-linux.md)):

- UEFI boot, Secure Boot off, GRUB working.
- Two empty GPT partitions on the ESP's disk, named `lazyos-system` and
  `lazyos-home` (any size). `gdisk`, command `c`, sets the names. Size
  `lazyos-system` for the suite you will install, because updates keep two
  or three releases there (decision 13):

  | Suite | Release size | `lazyos-system` |
  |---|---|---|
  | `base` (the desktop) | about 0.5 GiB | 4 GiB or more |
  | `full` (every optional app, WebKit browsers included) | about 1 GiB | 12 GiB or more |
  | `full` + SRB2Kart | about 1.5 GiB | 16 GiB or more |

### 2. Make the stick (today for `base`; `--suite` from U1b)

The **base** suite is the desktop:

```bash
python tools/run_demo.py --desktop --installer --build-only
```

The **full** suite adds every optional app a person would use (decision 14).
`--suite` runs each prerequisite build (`tools/xui/build.py`, the WebKit
builds in Docker, Doom, Quake, emusic, the Linux programs...) and sizes the
OS volume:

```bash
python tools/run_demo.py --suite full --installer --build-only
```

Add `--with srb2kart` to include SRB2Kart. Its game data is downloaded on
your machine at build time and must never be shared, so keep that stick to
yourself. The stick maker (`tools/boot/stick_gui.py`) and the launcher's
Simple tab have the same *Software: Base / Full* choice. Write
`target/lazyos-usb.img` to a USB stick ([`usb-stick.md`](usb-stick.md)).
Before U1b, a full stick is the long list of flags that
`python tools/run_demo.py --help` shows (`--lazyrad --pictures --lazyweb
--webkit --lazywebplus --mail --lazysky --games --doom --quake ...`).

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
slot-aware loader on the ESP (see "Layout"). The dry run prints the stick's
suite, its release size and the partition size it recommends. The installer
refuses a partition smaller than one release plus 1 GiB, and warns below the
recommended size.

**Already installed with an older `lazyos-install`?** Make a new stick (U0
and U2 or later), copy **its** `lazyos-install` to `/tmp` as above (the old
binary cannot convert), and run its `update` once (step 4c). It converts the
volume in place (a rename on the same volume, nothing copied; `/conf`,
`/apps`, `/logs` and `/home` untouched), installs the new loader and refreshes
the GRUB snippet. That is the only update that needs Linux.

### 4. Deploy an update

Build a release bundle on the dev machine (U1):

```bash
python tools/update/build.py --suite full --key ~/.lazyos/update.sec
```

This writes `target/lazyos-<release>-full.lzu`, a signed bundle of the
kernel, `/system` (core services and core apps of that suite included) and
`/docs/os`. `--suite` defaults to the suite of the image in `target/`;
A `full` bundle always carries SRB2Kart's engine and a signed, pinned
reference to its data, never the data itself; a machine installed
`--with srb2kart` links or downloads the data while staging (decision 15). Releases tagged `v*` are built and signed by CI
instead ("Release pipeline"), one bundle per suite, and the key's public half
is what the installed system trusts. `updated` keeps the machine on its
suite: it picks that suite's bundle from the feed, and refuses a bundle that
would drop an installed feature unless told to (`--suite base`). Moving a
`base` machine to `full` is an ordinary update, `updatectl install --feed
--suite full`, with the same trial and fallback. Then pick one route:

**a. From LazyOS, a file (U3).** Copy the bundle anywhere LazyOS can read
(`~/Downloads`, a FAT stick at `/media/<label>`, a network drive), then in the
Terminal:

```bash
updatectl install ~/Downloads/lazyos-0.4.0-full.lzu
```

```bash
updatectl restart
```

`install` asks for an administrator's password (`elevd`), checks the
signature, stages the release beside the running one, checks every file's
digest, and marks it to boot on trial. `restart` asks `init` to shut down
cleanly and tells GRUB to come back to LazyOS for the trial (decision 6).
Without it, a reboot lands on GRUB's default, usually Linux; the update then
waits until LazyOS is next picked from the menu. Settings > Updates does the
same with buttons: *Install from file...*, then *Restart now*.

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

- The release boots **on trial**. During a trial GRUB shows its menu for 5
  seconds with LazyOS selected, so Linux stays one arrow key away. Once the
  desktop has come up and `healthd` reports `ok` for 60 s, `updated` marks it
  good and the toast says "Updated to 0.4.0".
- If it panics (it restarts by itself after 10 s), hangs (press the power
  button) or never reaches a healthy desktop, it is tried three times. The
  fourth boot runs the previous release, with settings restored to their
  pre-update copy, and the notice says why. Only if that fails too does the
  next boot go to Linux, from where step 4c repairs it.

To see what happened, run `updatectl status`. To go back to the previous
release, run `updatectl rollback` and then `updatectl restart`. A rollback
is also a trial, so it cannot strand the machine either:

```bash
updatectl status
```

```bash
updatectl rollback
```

`updatectl list` shows the kept releases (current, previous, base) and their
state.

**GRUB without the snippet.** On a distro whose GRUB ignores `/etc/grub.d`
and `custom.cfg` (a BLS-only setup), `lazyos-install` says so at install
time. Pick LazyOS from the menu on each trial boot; fallback still works,
because the loader counts the tries.

### 6. Applications

Core apps follow the release: they are in the bundle's `/system/packages`,
and `pkgd` upgrades `/apps` from it at the first boot of a new release (it
does this today) and, from U3, back down after a fallback or rollback. Apps you
installed yourself update without a restart. `pkgctl install` of a newer
version works today; `pkgctl rollback` comes with U5:

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
directions; app rollback; the migration of existing installs; installing and updating
the optional **full software suite** (every optional app, WebKit and games
included), and moving between suites, with every script that builds an image
or a bundle taking the same suite name.

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
| Software suites | every optional app has a build switch, a `run_demo.py` flag that runs its prerequisite build and a launcher control; `osvolume.py` sizes the OS volume for WebKit and SRB2Kart; no named set of them | `tools/suites.toml` and `--suite` everywhere (decision 14); release limits in the release; external entries for SRB2Kart data (decision 15) |
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
   `RELEASE`. `/releases`, its files, `/system/docs` and `/logs/update.log`
   are new `libs/fhs` constants. Everything outside `/releases` is state and
   no update touches it. `/releases` is readable by everyone (like `/system`
   today) and writable only by `updated` (decision 10).

   **Development builds.** `cargo build` keeps updating an image in place,
   but into one release named `dev` that is never a link source and never
   linked into: the build replaces a file by writing a new inode and renaming
   it over the old name, never by overwriting it. That rule holds for every
   writer of `/releases`, so a shared inode is never changed. A signed
   bundle's id is unique (two different trees under one id are refused); the
   `dev` tree is the only one that is rebuilt.
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
   loading anything: find the system partition by its **GPT unique partition
   GUID**, which the installer writes to `\EFI\lazyos\system.id` (GRUB, in
   decision 6, finds the same partition by its ext2 filesystem UUID, which
   the installer substitutes into the snippet). It reaches the partition
   through `BlockIO`, opens it read-only with `libs/ext2fs`, reads `bootctl`
   and applies the rules (decision 5). It then writes the new control record
   **into the file's existing blocks** and calls `FlushBlocks`. That is a raw
   `BlockIO` write of a sector the inode already maps: no allocation, no
   metadata change, safe on an unclean volume. Finally it loads that
   release's kernel. Loading `boot.json` and the config `ramdisk` from its
   own directory (`boot_dir.rs`) is unchanged. Anything unreadable (no `system.id`, an ext2 error, both `bootctl`
   copies bad, a missing kernel) loads the ESP's rescue kernel, which boots
   its pinned base release. Every decision is printed on the loader's screen
   and recorded in `bootctl` for the kernel to log (`UPDATE:BOOT:<id>
   TRY:<n> REASON:<..>`).
5. **Boot control (`libs/bootctl`, `no_std`, shared by loader, kernel,
   `updated` and `lazyos-install`).** Two 512-byte records in `bootctl`, at
   byte 0 and byte 512. Each is one sector whatever the ext2 block size, and
   a sector write is the unit a power cut cannot tear. Each record holds:
   magic, version, sequence, CRC-32,
   and up to four entries `{release id, state, tries_left}` where state is
   `good`, `trial` or `bad`, plus `next` (the release to boot), `booted`
   (what the loader chose this time), `fell_back` (and from what), `restore` (set by a rollback, cleared by `init`) and the security floor. A
   writer writes the copy with the older sequence, then the other; a reader
   takes the newer valid one. Every writer in LazyOS (`updated`, the build)
   `fsync`s the file before it reports success: a record that reached only
   the cache did not happen. Rules, applied by the loader:
   - `next` is `good`: boot it;
   - `next` is `trial` with `tries_left > 0`: decrement, boot it;
   - otherwise mark it `bad`, set `next` to the newest `good` entry, set
     `fell_back`, boot that;
   - no `good` entry: the rescue kernel.

   With 3 tries a release that never comes up is booted three times and the
   fourth boot runs the previous good release, so GRUB's credit (decision 6)
   is one more than the tries: 4.

   Every kernel carries its release id, built in, and mounts
   `/releases/<own id>/system`. It does not trust `booted` for that, so the
   rescue kernel, loaded when `bootctl` could not be read, still finds its
   tree. A missing tree is a panic that names it. `booted` and `fell_back`
   are what `init` and `updated` act on and what the kernel logs.
6. **Coming back to LazyOS after a restart (GRUB).** GRUB's default stays
   the person's choice (usually Linux), so a plain reboot from LazyOS would
   land in Linux and a trial could never retry. The installer's snippet (in
   `40_lazyos`, or in `custom.cfg` where there is no `/etc/grub.d`, never
   both) gains lines outside the `menuentry`, which already carries
   `--id lazyos`. GRUB runs them while reading `grub.cfg`, after
   `00_header` has set `default` and `timeout` (Ubuntu's `recordfail`
   included), so they win for this boot only. `saved_entry` is never
   written:

   ```
   insmod part_gpt
   insmod ext2
   set lz=0
   search --no-floppy --fs-uuid --set=lzsys @SYSTEM_FS_UUID@
   if [ -n "$lzsys" ]; then
     load_env -f ($lzsys)/releases/grubenv lazyos_boots
     if   [ "$lazyos_boots" = 4 ]; then set lazyos_boots=3; set lz=1
     elif [ "$lazyos_boots" = 3 ]; then set lazyos_boots=2; set lz=1
     elif [ "$lazyos_boots" = 2 ]; then set lazyos_boots=1; set lz=1
     elif [ "$lazyos_boots" = 1 ]; then set lazyos_boots=0; set lz=1
     fi
     if [ "$lz" = 1 ]; then
       save_env -f ($lzsys)/releases/grubenv lazyos_boots
       set default=lazyos; set timeout_style=menu; set timeout=5
     fi
   fi
   ```

   GRUB script has no arithmetic, hence the ladder (Fedora's boot counting
   does the same). Any other value, including a corrupt one, matches no rung
   and leaves GRUB's own choice alone. `save_env` rewrites an existing env
   block in place. `grubenv` is created by the installer, exactly 1024 bytes
   (within one ext2 block, so never fragmented), and only ever overwritten
   in place. `updatectl restart` (and LazyOS's own *Restart*, which then
   means "restart LazyOS") writes `lazyos_boots=4` and `fsync`s it before
   asking `init` to reboot. `updated` writes `0` once the boot is marked good,
   or once a fallback release has come up. So:
   - a restart returns to LazyOS;
   - a failing trial gets three boots, and the fourth credit lands on the
     loader's fallback (decision 5);
   - the worst case ends in Linux;
   - the menu is shown for 5 s on these boots, so Linux stays reachable.

   *Power off* never sets the counter, so a cold boot goes to the person's
   default as before. Whether `save_env` works on our partition with the
   distro's GRUB is the first thing U2 proves (U2a); the whole
   return-to-LazyOS path depends on it, and the fallback without it is the
   menu, as the guide says.
7. **"Good" means the session came up** (revision 1, unchanged). `updated`
   marks the running kernel's own built-in release id `good` (never
   `bootctl.booted`, which a rescue boot leaves stale; when the two differ it
   marks nothing and reports the rescue boot) once `healthd`'s summary is `ok` and the
   session has run 60 s, and `fsync`s `bootctl` and `grubenv` before it
   reports `UPDATE:GOOD`. A trial boot reboots 10 s after a panic instead of
   halting (the panic screen shows the countdown); a hang needs the power
   button until a watchdog driver exists for the box. Three tries.
8. **A signed bundle, trust carried by the running release.** An `.lzu` is a
   zip under `lazypkg`'s container rules (no zip64, stored or deflate, path
   checks) holding `manifest.toml`, `manifest.sig` and the tree:
   `kernel-x86_64` and `system/...`. `manifest.sig` is a raw Ed25519
   signature over the manifest's exact bytes with a key id. The netboot
   tools sign this way (`tools/netboot/`); U1 writes the byte format down
   once for the Rust verifier and the Python signer. The manifest
   (`deny_unknown_fields`) names:
   - the release and the channel;
   - the minimum release it may be installed over, and the layout version;
   - the loader it needs (`requires_loader`);
   - a `security_floor`;
   - an `expires` time for feed metadata;
   - every file, with mode, owner, size and SHA-256.

   Trusted keys are `/system/etc/update/keys/*.pub` of the **running**
   release, so a release can rotate them for the next, and only through its
   own signed manifest. The signature is checked before anything else is
   read. Downgrades:
   - a release older than the running one is refused unless
     `--allow-downgrade`;
   - one below the floor is refused always. The floor is the highest
     `security_floor` of any release marked good, kept in `bootctl`, so a
     rollback cannot reopen a fixed hole.

   A feed whose `expires` has passed is refused, so a stale mirror cannot
   freeze a machine on an old release unnoticed. A release image never
   trusts a development key: `release.yml` fails a build whose key directory
   holds one.
9. **Staging is copy-or-link, then one rename.** `updated` creates
   `/releases/<id>.partial.<n>/` (`<n>` counts attempts, so a retry never meets an earlier one). For each manifest entry it hard-links the file
   of the newest kept signed release whose digest, mode and owner all match,
   or else extracts the file from the bundle. It then `fsync`s every new file
   and directory, reads every new file back against its digest, renames the
   directory to `/releases/<id>`, `fsync`s `/releases`, and only then writes
   `bootctl` (`next = id`, `trial`, 3 tries).
   - A power cut before the rename leaves a `.partial`. The next `updated`
     start deletes it on a clean volume. Until then the space check does not
     count it, and `updatectl status` shows the space it holds.
   - A power cut after the rename leaves an unbooted release, which
     `updatectl list` shows.
   - Files are never modified once staged: the mount is read-only, and every
     writer replaces by new inode and rename (decision 1). That is what makes
     the hard links safe.

   **An unclean volume** (the kernel has no fsck) is normal here: a hung
   trial is recovered by the power button. Staging is allowed on it **if**
   the library's commit order leaves only leaked blocks and inodes, counters
   and link counts that are too high. That is an assumption until U1's
   crash-prefix tests prove it: they cut power at every write of a link, an
   unlink and a rename of a hard-linked inode. If they show a count can be
   left too low, staging also waits for a clean volume, and the boot-time
   repair moves from "Later" into U3. Deleting is the operation that a wrong
   count makes dangerous, so pruning old releases and reclaiming `.partial`
   always wait for a clean volume.
   `updatectl status` then says so; repair is the next build or
   `lazyos-install check --repair` from Linux (new in U1, the repair code
   `os_update.rs` already runs). A boot-time repair is "Later".
10. **`updated`: one service, one directory.** It runs as `_update` with only
    `CAP_CHOWN`, which it needs because release files are owned by root
    (manifest owners are limited to 0 and the system uids `passwd` defines,
    and setuid and setgid bits are refused). The kernel's file policy
    confines that capability: `_update` may `chown` only under `/releases`,
    and only to those uids. The kernel's file policy lets
    only `_update` create, rename or delete under `/releases` (and write in
    place to `bootctl` and `grubenv`), and nothing writes under a mounted
    `/system`. It serves `os.lazy.update.v1` (`idl/update.midl`, generated
    with `midlc`): `Status`, `Check`, `Install(file | feed)`, `Rollback`,
    `List` and `Restart`, plus `MarkGood` only from its own health watch.
    Its clients are `updatectl` (a native user bin, so it goes on the
    kernel's native execve list) and Settings. `Install`, `Rollback` and
    `Restart` go through `elevd` (`update.install`, `update.rollback`,
    `update.restart`, an administrator's prompt) like every privileged
    change. Core apps change only through provisioning, never through
    `pkgctl`. It logs to `/logs/update.log` and traces on a new `UPDATE`
    channel (`tools/trace/catalog.py`).
11. **Settings survive a way back.** `/conf/.release` names the release that
    last ran with this `/conf`. `init` acts before it starts `confd`, keyed
    on the kernel's own built-in id and on `bootctl`, never on comparing
    version numbers (dev ids have no total order):
    - **A way back.** The loader set `fell_back`, or `updatectl rollback`
      set `restore` in `bootctl`, and `.release` is not the running id.
      `init` restores `/conf/.snap/<running id>/`, writes `.release` = the
      running id and only then clears `restore`. If no such snapshot exists,
      it keeps `/conf` and logs that it did not restore.
    - **A move forward.** `.release` is not the running id and nothing asks
      for a way back: a new release, an `--allow-downgrade` install or a
      suite change. `init` snapshots `/conf` minus `/conf/.snap` into
      `/conf/.snap/<.release>.tmp/`, `fsync`s it, renames it to
      `/conf/.snap/<.release>/` (replacing an older snapshot of that
      release, left by an earlier attempt) and only then writes `.release`. If a power
      cut comes before the stamp, the next boot finds the complete snapshot
      already there. It was taken before any `confd` of the new release ran,
      so `init` keeps it and writes the stamp. A leftover `.tmp` is deleted.
    - **The rescue kernel** (`bootctl` unreadable) neither snapshots nor
      restores. It runs on `/conf` as it is and says so: it is a way to
      repair the machine (step 4c), not a release to live on. A `restore`
      flag stays set until a normal kernel boots and acts on it.

    A release may add keys and must keep reading the previous release's
    keys. Snapshots are kept for every release still on disk, the pinned
    base included. Secrets `keyd` sealed after the snapshot are lost with a
    restore, and the notice says so. The promise is about settings and core
    apps: user apps (`/apps`, origin `user`) and files in `/home` that a newer
    release changed are not rolled back.
12. **Core apps go both ways.** Provisioning today upgrades a core row only
    upward and keeps a newer version "the user installed". U3 records who
    installed a row (`installed_by = provision | user`) so a row provisioning
    put there is brought to the shipped version in both directions, while a
    version the person installed stays.
13. **Space.** `updated` keeps the running release, the previous `good` one
    and the pinned base. It deletes others (oldest first), on a clean volume
    only, after a new release is marked good. Before staging it sums the
    sizes of the files it cannot link and refuses, with the numbers, when the
    volume lacks them plus a 64 MiB margin. Unchanged files cost nothing (hard
    links), so a typical update of the `full` suite needs well under its 1
    GiB. The worst case is three releases that share nothing: three times
    the release size, plus `/apps` (core apps are extracted there too),
    `/logs` and `/conf`; that is where the guide's partition sizes come from.
    `lazyos-install` prints the recommendation and refuses less than one
    release plus 1 GiB. On a partition between the two, the install works and
    a later update may be refused with the numbers.
14. **Software suites: one list, read by every script.** A *suite* is a
    named set of the image's build switches (`docs/build-switches.md`).
    `tools/suites.toml` is the one definition, and `tools/suites.py` (pure,
    host-tested) reads it for `run_demo.py --suite`, the launcher and stick
    maker (`tools/lazygui/catalog.py`), `tools/update/build.py`, `preflight`
    and `release.yml`. The suites:
    - **`base`**: the desktop profile (`LAZYOS_DESKTOP=1`) and its default
      core apps.
    - **`full`**: `base` plus every optional app a person uses, and what each
      needs:
      - networking and HTTPS (`NETD`, `TLS`);
      - LazyRAD, the MOD player and the Picture Viewer;
      - LazyWeb, the WebKit browser and LazyWeb+;
      - Mail and LazySky;
      - the games (`GAMES`: LazyGolf, NES) and the Doom, Quake and emusic
        packages;
      - the Linux programs.

      It also carries the limits those apps need (`--big-app`, the 8G
      user-memory quota) and the OS size `tools/lazygui/osvolume.py` already
      computes.
    - **Options**, added with `--with <name>`: `srb2kart` (decision 15), and
      later anything that should not be in `full` for everyone.

    Test and developer switches are never named by a suite: the UI probe,
    label trace, the `jsc`/`webkit-deps`/`webkit-port` console proofs, the
    tray demo, the Wi-Fi simulator and `dbgd`. A switch a named one implies
    (WebKit brings the port's processes) is fine. A suite names switches, not build steps. Each switch
    already has a `run_demo.py` flag that runs its prerequisite build, and
    `--suite` sets those flags, so a new optional app joins a suite with one
    line and inherits its build step.

    The release records its suite, its options and the switches it was
    built with in `RELEASE`. An option is a part of a suite's bundle that a
    machine may or may not stage, not a separate bundle. `full` always
    carries the SRB2Kart engine and package (redistributable), and lists its
    data as optional external entries (decision 15). A machine with
    `srb2kart` stages them; one without skips them. So the feed needs only
    one bundle per suite. Suite rules for updates:
    - **The feed** lists one bundle per suite.
    - **`updated` stays on the installed suite** and its options. A bundle
      missing a feature the running release has is refused unless the person
      names the new suite (`--suite base`). A core app the new suite drops
      becomes a removable user app; that rule exists today.
    - **Changing suite** in either direction is an ordinary update, so it is
      trial-booted and can fall back.
    - **Release limits.** `limits::apply_config` runs when `bootcfg` reads
      `lazyos.cfg`, before any disk is mounted, and some keys are consumed
      there (`task_max`, `scratch_max`, `shared_buffer_max`, the DMA pool).
      Only the keys that are safe to change later move into the release
      (`/system/etc/limits.cfg`): `heap_max` (the heap grows on demand),
      `fd_max`, `stack_size` and the two quotas. The build refuses any other
      key there. After mounting `/system` the kernel applies the release's
      lines, then the ESP's lines for those keys again, so the machine
      owner's line still wins. After a fallback the older release's file
      applies. The early keys stay in the ESP's `lazyos.cfg`, which
      `lazyos-install` writes from the stick's suite. A suite change that
      needs a different early key is refused by `updated`, which names the
      `lazyos-install update` that would set it. `full`'s `--big-app`
      profile is only quotas and heap, so adding WebKit by an update needs
      nothing from Linux.
15. **Data that must not be redistributed (SRB2Kart).** Kart Krew's game data
    (~220 MB) may be downloaded by the person but never shipped (AGENTS.md),
    so no bundle and no published image carries it. Its manifest entries
    are *external*: path, size, SHA-256 and the source URL. They sit in the
    **signed** manifest, generated from the pins in
    `tools/srb2kart/fetch.py`, so a download is checked against signed
    digests. To stage one, `updated` tries in order:
    1. hard-link the file from a kept release when the digest matches (the
       usual case, since the data rarely changes);
    2. else download it from Kart Krew's own release over HTTPS and check
       the digest;
    3. else, offline, refuse with the file to fetch.

    `lazyos-install` copies the data from the stick the person built, which
    already carries it. The `full` job in `release.yml` never runs
    `fetch.py`, and the bundle build refuses a tree holding external bytes.
    Wi-Fi firmware (`tools/firmware/manifest.txt`, never committed, each file
    with the licence file that must travel with it) goes in a bundle only
    with that licence beside it. Its redistribution terms are to be checked
    per file before the first release that ships it; failing that, it uses
    external entries too.

## Architecture

```
 release CI (or a dev host)
   build ─► tree (kernel + system/) ─► manifest.toml (digests) ─► sign ─► lazyos-<id>.lzu
           └─► feed.json per channel on GitHub Releases

 updated (LazyOS)                      lazyos-install update (Linux)
   file │ feed │ push                    stick
     └──────────────┬─────────────────────┘
          libs/lzupdate: verify signature ─► stage /releases/<id>.partial (link or extract)
          ─► read back ─► rename ─► bootctl: next=<id> trial tries=3
   updatectl restart ─► grubenv lazyos_boots=4 ─► init.Shutdown(reboot)

 firmware ─► GRUB: grubenv says LazyOS ─► chainloader \EFI\lazyos\BOOTX64.EFI
 loader   ─► ext2 read of lazyos-system ─► bootctl rules ─► tries-1 written in place
          ─► /releases/<id>/kernel-x86_64        (or the ESP rescue kernel)
 kernel   ─► / = lazyos-system, /system = /releases/<id>/system (ro), /home
 init     ─► /conf snapshot (trial) or restore (went back) ─► pkgd reconcile (up or down) ─► desktop
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

As revision 1 (decided 2026-10-04): `.github/workflows/release.yml`
builds every `v*` tag. From U1 it also builds one `.lzu` per suite (`base`, `full`; never with SRB2Kart data, decision 15; the `full` build reuses the WebKit Docker volume between runs); a separate `sign`
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
| **U0** Layout and migration | `/releases/<id>/` layout written by the image build (the `dev` release, replace by rename) and by `lazyos-install` (fresh install, and conversion of an existing volume by rename); `SubtreeFs` and the `ro,nosuid` `/system` mount; the kernel mounts its own built-in release id's tree; `/docs/os` to `/system/docs`; new `libs/fhs` constants; mutable `/system/etc` files moved to `/conf`; `python tools/fhs/check_literals.py` green | `fs` suite: subtree mount reads, `EROFS` on every write path, a missing tree refused by name, a soak of mount and unmount; `cargo test -p lazyos-install -p build-support-tests`: conversion keeps `/conf`, `/apps`, `/logs` byte-identical, a rebuild never writes into an inode with two links; harness: install an old-layout volume, convert, boot, the desktop and a confd key are there |
| **U1** Bundles and staging | `libs/lzupdate` (manifest, container via `lazypkg`'s reader, Ed25519 verify with the signature format written down, staging with links, `.partial` reclaim, space check, floor), shared by the build, `lazyos-install` and `updated`; `tools/update/build.py`, `keygen.py`; `LAZYOS_UPDATE_KEY` makes `cargo build` write the `.lzu` (`run_demo.py --update-key`, the launcher's Advanced tab through `tools/lazygui/catalog.py` and `test_catalog.py`); the `sign` job; `lazyos-install update` stages instead of overwriting; `lazyos-install check [--repair]` | `cargo test -p lzupdate`: wrong key, a flipped byte per file, unknown manifest field, downgrade, below the floor, expired feed, path escape, zip64, oversize, setuid refused, mode or owner mismatch never linked; a power cut at every write of staging (ext2fs crash prefixes) leaves the running release bootable, never a link count too low, and the next start reclaims; fuzz on manifest and container |
| **U1b** Suites | `tools/suites.toml` and `tools/suites.py`; `run_demo.py --suite NAME [--with OPT]` setting the existing per-feature flags (so their prerequisite builds run) and the OS size; the *Software: Base / Full* control on the launcher's Simple tab and in the stick maker, through `catalog.py` (`build_env`, `build_plan`); `tools/update/build.py --suite`; the suite and switches recorded in `RELEASE`; release limits in `/system/etc/limits.cfg` read by the kernel after the `/system` mount; external manifest entries and the SRB2Kart fetch in `lzupdate`; `lazyos-install` sizing advice from the stick's suite; `release.yml` building `base` and `full` | `python tools/test_suites.py`: every switch a suite names exists in `build-switches.md` and has a `run_demo.py` flag, no test switch is in a suite, `full` includes every app switch not marked test-only (a new app must choose), the OS size matches `osvolume.py`; `tools/lazygui/test_catalog.py` for the control; `cargo test -p lzupdate`: an external entry linked, fetched (digest checked) or refused offline, a bundle carrying external bytes refused by the build; kernel `mount_suite`: release limits applied, an ESP line wins, an early key in `limits.cfg` refused, the older release's limits after a fallback; `tools/update/run.py --suite full` (nightly, heavy): install `base`, update to `full`, LazyWeb+ and Doom open, fall back to `base` with `/conf` restored |
| **U2a** GRUB spike | the snippet against real GRUB: Ubuntu 2.14 on the box and `grub-mkstandalone` in `tools/install/run.py --via grub`; `load_env` and `save_env` on the ext2 partition, `default` and `timeout` overriding `00_header` and `recordfail`, both the `40_lazyos` and the `custom.cfg` placement | the countdown 4, 3, 2, 1, 0 read back from `grubenv` across five boots, then the default again; a corrupt `grubenv` leaves the default alone. Blocks U2's GRUB half, not the loader |
| **U2** Loader, control, GRUB | `libs/bootctl`; the loader's ext2 read, rules, in-place write and `FlushBlocks` (`vendor/bootloader/uefi`); `system.id`; the rescue path; the snippet in `40_lazyos.in` and `custom.cfg` and `grubenv`, written by `lazyos-install` (and removed by `uninstall` with `system.id`); the trial-boot panic reboot | `cargo test -p bootctl`: torn copies, CRC, sequence wrap, every rule, a seeded soak of random power cuts over the loader's `BlockIO` shim under the ext2 checker; harness under OVMF through real GRUB: a release whose kernel panics is booted three times, then the previous one boots with `UPDATE:FALLBACK` and `grubenv` back at 0; when every release fails, `lazyos_boots` ends in Linux; a wiped `bootctl` boots the rescue kernel on its own tree; Linux's entry and files byte-identical |
| **U3** `updated`, `updatectl`, Settings | `idl/update.midl`, the service, `_update` and its file policy, `elevd` actions `update.install`/`rollback`/`restart`, the `/conf` snapshot and restore in `init`, `pkgd` two-way core reconcile, Settings > Updates (file install, status, rollback, restart), the `UPDATE` trace channel, `updatectl` on the native execve list, `run_demo.py --update-dev-key` and its launcher control | harness: install N+1 from `/home`, restart, `UPDATE:GOOD`, `/system` is N+1, core apps upgraded, settings kept; kill QEMU mid-staging, N still boots and `updatectl status` explains; staging after an unclean stop works, pruning waits; rollback returns to N with core apps and `/conf` restored; account attack harness: the session cannot write `/releases` or call `Install` without the prompt; screenshot of the Settings page read |
| **U4** Feed and push | the feed client (rustls, system CA bundle), channels, a daily check and a notification, `tools/update/push.py` and the `dbgd` path for the dev loop | a local HTTPS server under the test CA (as `tools/net/tls_run.py`): found, downloaded, installed, booted; truncated download, wrong digest, stale feed refused and explained |
| **U5** Apps | `pkgd` keeps the previous version directory of an upgraded user app, `pkgctl rollback`, the Installer's "Upgrade x to y" text, an optional per-app feed in the manifest | `cargo test -p pkgstore`: upgrade, rollback, a power cut at every step; session: install 1.2, upgrade to 1.3 while it runs, relaunch, roll back |
| **Later** | file-level delta bundles (only the files the target release lacks, made possible by U1's links), background download, an install-at-a-time setting, a watchdog on the box, a boot-time repair of an unclean volume, a Multiboot2 GRUB entry that loads the kernel from ext2 directly (grub-install-plan "Later") | as each lands |

**Order on the box:** U2a first (one evening, it decides decision 6); U0
and U2 together make the first `lazyos-install update` from Linux the last
mandatory one; U1 and U3 give updates from a file
with fallback; U1b gives the full suite and suite changes; U4 gives one click; U5 is independent and can go first.

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
   install uses. That is unproven here, so U2a settles it before the GRUB half
   of U2 is built. A distro that ignores the snippet (BLS-only) still boots
   LazyOS from the menu, and `updatectl restart` then says the restart will
   need the menu. On distros that set `GRUB_DEFAULT=saved`, the snippet must
   not disturb `saved_entry`. U2a checks that too.
4. **`/system/etc` writers.** The audit in U0 may find a service that relies
   on writing there; each one moves to `/conf` or the plan needs a writable
   overlay for that file.
5. **Who may update.** This plan says an administrator through `elevd`. A
   single-user box may want "any session user"; that is a policy line in
   `elevpolicy`, not a design change.
6. **Release ids.** Bundles use semantic versions from the root
   `Cargo.toml`, plus the commit for dev bundles (`0.4.0-dev.<sha8>`), and are
   compared by `lazypkg`'s version rules. Two different trees with one id are
   refused. The in-place `cargo build` tree is always `dev` (decision 1).
7. **`keyd` and the `/conf` restore.** Restoring `/conf` brings back the
   sealed secrets as they were before the trial. A secret stored during a
   failed trial is lost. That is acceptable for v1; the notice names it.
8. **Hangs without a watchdog** cost a press of the power button per try,
   and each leaves the volume unclean (decision 9). A watchdog driver for the
   box, and a boot-time repair, are "Later".
9. **The size of `full`.** A `full` bundle is large (the WebKit programs
   alone are over 100 MB each) and the CI job that builds it is slow, which
   is why it reuses the WebKit build volume and why file-level deltas, the
   first item of "Later", matter most for this suite. A machine on a slow
   link can stay on `base`.
10. **What belongs in `full`.** `tools/test_suites.py` makes every new app
    switch choose `full`, an option or test-only, so the suite cannot drift
    from the app list silently; the choice itself is a review question.
