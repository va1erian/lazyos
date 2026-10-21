# Plan: updating an installed LazyOS

> **Status: draft proposal, revision 3 (2026-10-21).** Nothing here is built
> beyond what "What exists" says. **Revision 3 makes the whole OS (kernel,
> low-level services, data, core apps) a set of versioned system packages,
> managed with an apt/dnf-style `updatectl`** (decisions 16-27, the Package
> track in "Phases"), while keeping revision 2's atomic release directories,
> trial boots and fallback as the activation mechanism: packages for identity
> and download, generations for switching and rollback (the ostree/Nix model,
> not in-place apt). Revision 2 consolidated revision 1 (#565,
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

### 7. Everyday package commands (the Linux-like part)

From revision 3 every file under `/system`, and the kernel, belongs to one
**system package**, and `updatectl` is LazyOS's `apt`/`dnf`. Same habits, with
one difference you will notice: changes are atomic generations, so an
`upgrade` that goes wrong falls back by itself instead of leaving a
half-upgraded system.

| You want to | Debian/Fedora | LazyOS (`updatectl`) |
|---|---|---|
| refresh the index | `apt update` | `updatectl check` (fetches the signed snapshot index only) |
| see what would change | `apt list --upgradable` | `updatectl list --upgradable` (names, versions, size to download, and what must restart) |
| upgrade everything | `apt upgrade` | `updatectl upgrade`, then `updatectl restart` if it says a restart is needed |
| add / remove software | `apt install foo` / `apt remove foo` | `updatectl install lazyos-golf` / `updatectl remove lazyos-golf` |
| search and inspect | `apt search`, `apt show` | `updatectl search nes`, `updatectl show lazyos-netd` (version, dependencies, files, restart class) |
| who owns this file | `dpkg -S /bin/ls` | `updatectl which /system/bin/netd` |
| check installed files | `debsums`, `rpm -V` | `updatectl verify [pkg]` |
| keep a version | `apt-mark hold` | `updatectl hold lazyos-netd` |
| what needs a restart | `needrestart` | `updatectl needs-restart` (exact: each package declares it) |
| history / undo | `dnf history`, `dnf history undo` | `updatectl history`, `updatectl rollback` |
| apps (not system) | `flatpak install` | `pkgctl install foo.lzp` (unchanged; sandboxed) |

```bash
updatectl check
```

```bash
updatectl list --upgradable
```

```bash
updatectl upgrade
```

```bash
updatectl needs-restart
```

`upgrade` downloads only the packages whose digest changed, stages them beside
the running release, and says one of five things per package: **nothing to
restart**, **applies at next launch**, **services restart now** (`netd`, `audiod`
... restart with no reboot), **log out and in** (the desktop session), or
**reboot** (kernel, `init`, `messengerd`, `confd`). A reboot-class update is
the trial boot of steps 4-5; the others switch live and fall back the same
way if the units do not come back healthy. You cannot install a random mix of
base package versions (no "partial upgrade"): `install`, `remove` and `hold`
produce a new snapshot of the same signed packages and check that it is
consistent before anything is touched.

## Goal and scope

Move an installed machine from release N to N+1 (kernel, core services, core
apps, docs) from inside LazyOS, with at most one restart, and never leave it
unbootable: a power cut while staging, a bad download, a kernel that panics on
this hardware or a desktop that never comes up all end with release N running
again, on its own, with settings, apps and files intact. Update user-installed
apps without a restart, with a way back. Make the software itself legible the
way a Linux distribution is: every file has an owning package with a version
and dependencies, an update downloads only what changed, and the cost of
applying it (nothing, a service restart, a reboot) is known beforehand.

**In:** signed release snapshots made of system packages; staging from a file,
a stick, the release feed or a dev push, by package; trial boots with automatic
fallback; manual rollback; `updatectl`, `updated`, a Settings page; package
queries (`list`, `show`, `which`, `verify`, `history`, `hold`,
`needs-restart`); installing and removing optional packages; live activation of
leaf packages; kernel-only updates under a syscall-ABI window; core-app
reconcile in both directions; app rollback; the migration of existing installs;
installing and updating the optional **full software suite** (every optional
app, WebKit and games included) as a meta-package, and moving between suites,
with every script that builds an image or a bundle taking the same suite name.

**Out (for now):** updating the loader or the GRUB entry from inside LazyOS
(a rare `lazyos-install update` from Linux does that, see "The ESP"), Secure
Boot, firmware capsules, third-party *system* packages (only the project's key
signs them; third parties ship sandboxed `.lzp` apps), a source/ports tree, a
dependency solver with alternatives, Linux-side changes of any kind.

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
| System packages | none: `build.rs` and `build_support/*_embed.rs` place a flat file list from env switches; `/system/.image-manifest` is the only record, with no owner, version or dependency per file | one owner per file, per-package metadata, the image assembled *from* packages (decisions 16-17) |
| Services | `init`'s `MANIFEST: &[ServiceSpec]` const (`user/src/bin/init/state.rs`) and `service_creds.rs`, compiled in | unit files shipped by each package, read from `/system/units` (decision 19) |
| Compatibility contracts | MIDL is append-only within `.vN` with an interface hash checked at bind time (`docs/midl.md`, `docs/messenger.md` §11); no syscall ABI version; no state-schema versions | all three declared per package and checked before a snapshot is activated (decision 18) |
| Libraries | everything is statically linked; drivers are user space; the kernel has no modules | build-closure metadata so "libcrypto changed" names the packages it rebuilds (decision 24) |
| Reproducibility | core `.lzp` archives are reproducible (`tools/xui/core_packages.py`); binaries unverified | a CI check that two builds are bit-identical, otherwise package-level deltas do not exist (decision 24) |
| Data shipped separately in a distro | CA bundle, fonts, docs, Wi-Fi firmware, wallpapers all inside the image | independent `none`-class packages (decision 17) |
| Live service updates | `dbgd`'s `reload` (developer tool); `pkgd` for apps | a re-targetable `/system` mount plus unit restarts (decision 23) |

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
4. **Packages on top of release directories (revision 3). Chosen, together
   with 3.** Distributions split three ways: in-place transactional packages
   (apt, dnf: small downloads, `needrestart`, but partial upgrades, scripts run
   as root on the live system and no free rollback), whole images (ChromeOS,
   Flatcar: atomic, but one opaque blob), and functional generations (Nix,
   ostree: atomic *and* per-component). LazyOS takes the third: packages carry
   identity, versions, dependencies, signing, download and ownership; release
   directories carry activation, trial and rollback. Two apt habits are
   rejected on purpose: **no maintainer scripts** (what they do is data,
   decision 21) and **no partial upgrades** (the unit of upgrade is a
   snapshot, decision 20).

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

   The kernel mounts `/releases/<id>/system`, where `<id>` is the `release=`
   the loader passes in the boot info (revision 3, decision 22: a kernel file
   shared by two releases is one hard-linked inode and so cannot embed a
   release id). Only the **rescue kernel** carries a built-in id, the base
   release it pins, so it still finds its tree when `bootctl` could not be
   read. A missing tree is a panic that names it, as is a tree whose
   `requires.kernel-abi` the kernel does not provide. `booted` and `fell_back`
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
   (`deny_unknown_fields`) is the **snapshot** of decision 20 and names:
   - the release, its sequence number and the channel;
   - the minimum release it may be installed over, and the layout version;
   - the loader it needs (`requires_loader`);
   - a `security_floor`;
   - an `expires` time for feed metadata;
   - every package, with version, size and SHA-256 (revision 3). Each
     package's own manifest lists its files with mode, owner, size and
     SHA-256, so a bundle that carries the packages is still verified file by
     file, and a package is trusted only through the signed snapshot.

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
14. **Software suites: one list, read by every script.** *(Revision 3: the
    list is also compiled into the meta-packages `lazyos-base`, `lazyos-full`
    and one per option, so changing suite is `updatectl install`/`remove` of a
    meta-package and an option's external data is a package whose payload is
    external entries, decision 15. Everything below still holds.)* A *suite*
    is a named set of the image's build switches (`docs/build-switches.md`).
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

### Packages (revision 3)

16. **Two package kinds, one container.**
    - `.lzp` (exists): an *application*. Sandboxed label, permissions,
      per-user visibility, installed by `pkgd` into `/apps`. Unchanged.
    - `.lzs` (new): a *system package*. Lands in a release tree under
      `/system` (or is the kernel), owns files by path, may carry units that
      create services with uids and capabilities. Accepted **only** through a
      snapshot signed by a key in `/system/etc/update/keys` (decision 8).

    Both use `lazypkg`'s container rules (zip, no zip64, path checks, limits,
    the reproducible writer). `.lzs` has its own manifest, `[package]`,
    `deny_unknown_fields`, parsed by a new crate `libs/lzsys`. It is **not** a
    variant of the app manifest: an app manifest compiles into a *restricting*
    label, a system manifest compiles into *authority*, and the two parsers
    must never share a path that could confuse them.

    ```toml
    [package]
    name = "lazyos-netd"
    version = "0.4.0-3"            # upstream version + packaging revision
    kind = "service"               # kernel | boot | service | tool | data | meta | app
    summary = "Network stack service"
    activation = "restart-units"   # none | next-launch | restart-units | restart-session | reboot

    [provides]
    interfaces = ["os.lazy.net.v1"]

    [requires]
    kernel-abi = ">=3"
    interfaces = ["os.lazy.confd.v1", "os.lazy.logd.v1", "os.lazy.net.nic.v1"]
    packages = ["lazyos-confd >= 0.4"]      # only for what interfaces cannot say

    [state]
    owns = ["/conf/svc/netd", "sys/net/*"]
    schema = 2                              # decision 25

    [[unit]]
    file = "units/netd.toml"                # decision 19
    ```

    The packager appends the `[[file]]` list (path, mode, owner, size,
    SHA-256).
17. **Taxonomy and granularity: about sixty packages, cut by who can break
    whom and by restart cost.** A package is the largest set of files that
    always change together and share a restart class.

    | Kind | Examples | Activation |
    |---|---|---|
    | `kernel` | `lazyos-kernel` (one variant per feature set, e.g. `-trace`, never both installed) | `reboot` |
    | `boot` | `lazyos-boot`: loader, rescue kernel, config ramdisk (the ESP files) | applied by `lazyos-install` from Linux only; in a snapshot as a *requirement*, not a payload |
    | early `service` | `lazyos-init`, `-messengerd`, `-confd` (PID 1 and the `Once` units) | `reboot` |
    | `service` | one per daemon with its client tool: `-logd`, `-keyd`, `-accountsd`, `-elevd`, `-pkgd` (+`pkgctl`), `-netd` (+`netctl`, `ping`...), `-audiod`+`-sndd`, `-usbd`, `-timed`... | `restart-units` |
    | `tool` | BusyBox, `rhai`, `curl`/`wget`/`fetch`, `smb`, `dash`, `sqlite3`, `jq`, `rg`, the musl loader | `none` |
    | `data` | `-ca-certificates`, `-fonts`, `-terminfo`, `-wallpapers`, `-samples`, `-docs`, `-firmware-rtw88` (with its licence file) | `none` |
    | core `app` | `os.lazy.paint`, `os.lazy.writer`...: the payload is the existing reproducible `.lzp`, extracted by `pkgd` as today | `next-launch` |
    | `meta` | `lazyos-base`, `lazyos-full`, `lazyos-opt-srb2kart` (decision 14): only `requires` | none |

    A service and the command-line client that speaks its interface are one
    package (they share the IDL version); a driver and the service that
    supervises it are two. Splitting the early bootstrap set further is
    pointless (all of it needs a reboot), and merging a leaf service into it
    would charge a reboot for every network fix. `init` and `messengerd`
    (`Once`: the kernel's bootstrap channel can be claimed once per boot)
    cannot be restarted at all. `xuid` restarts the whole desktop, so its
    class is `restart-session` and the UI says so.

    **Activation classes** say what must happen for the new files to take
    effect. `updated` takes the maximum over the changed packages and reports
    it before the person commits (`needrestart`, but exact because the package
    declares it):

    | Class | Meaning | Mechanism |
    |---|---|---|
    | `none` | files are read on demand | stage and switch (decision 23) |
    | `next-launch` | a program started per use or per login | stage and switch; running instances keep their inodes |
    | `restart-units` | `init` restarts the listed units in dependency order | live switch, then `init` restarts them |
    | `restart-session` | the desktop session | log out and in |
    | `reboot` | kernel, `init`, `messengerd`, `confd`, any `Once` unit | the trial boot of decisions 5-7 |

    A class that is too cheap is a bug the harness catches (phase P5).
18. **Dependencies are contracts, and there are exactly five kinds.** LazyOS
    has no shared libraries, so a package never breaks because another's
    `.so` changed; the coupling is this short list, and it is what the
    resolver checks:
    1. **Syscall ABI** (`kernel-abi`). The kernel provides an integer and a
       compatibility floor (`5, compat-from 3`). Syscalls are append-only,
       never renumbered or retyped (as in Linux); a removal needs a
       deprecation window of two snapshots. A user-space package declares the
       highest ABI it uses and the lowest it runs on. The value lives in
       `libs/lazyos-sys`, and a kernel test (`abi_freeze`) fails when a
       syscall's number or argument layout differs from a checked-in table.
       This is what lets the kernel update without rebuilding the world and
       the world without a new kernel.
    2. **Messenger interfaces** (`os.lazy.x.vN`), *derived, not hand-written*:
       the packager reads which interfaces a binary serves and binds from the
       generated code (`idl/manifest.json` is the index). A requirement is met
       by the same `.vN` from a provider at least as new (append-only makes
       newer providers compatible). **Settled by spike S1:** the interface id
       checked at bind time is the FNV-1a hash of the `.vN` *name* only, so it
       never changes on an append but also says nothing about shape; it cannot
       be the compatibility test. The test is `tools/pkg/idl_compat.py`, a
       schema diff between two IDL trees, run by CI against the previous
       snapshot's tree.
    3. **Boot protocol** (`requires_loader`), checked against the ESP.
    4. **State schema** (decision 25).
    5. **Named packages**, `>=` only, no upper bounds, no alternatives, no
       conflicts beyond "a file has two owners", which is refused when the
       snapshot is *built*. Used sparingly.

    CI solves; the machine only **validates** a snapshot and the closure of a
    local add or remove. The on-device resolver never searches, which keeps it
    a few hundred fuzzable lines.
19. **Services are units, not code.** `init`'s boot table becomes data. A
    package's `units/<svc>.toml` carries what `ServiceSpec` and
    `service_creds.rs` hold today, plus what is now implicit:

    ```toml
    [unit]
    name = "netd"
    exec = "/system/bin/netd"
    args = ["demo=0"]
    after = ["confd", "logd", "devd"]       # today's `deps`
    restart = "on-failure"                  # always | on-failure | once
    [identity]
    user = "_net"                           # a system uid from a [[user]] fragment
    capabilities = ["CAP_NET_ADMIN"]        # a closed list; anything else is refused at build
    label = "svc:netd"
    [state]
    dirs = ["0750 _net /conf/svc/netd"]     # ensured by init before exec
    ```

    `/system/units` is in the read-only release, so a unit can only come from
    a signed snapshot: that is the whole answer to "a package may create a
    root-equivalent daemon". `init` reads the units at start (the rescue
    kernel uses a compiled-in bootstrap list) and re-reads them on
    `init.Reload` for live activation. System users and groups are `[[user]]`
    fragments merged into the release's `passwd` view at assembly; human
    accounts stay in `/accounts/db`, and a snapshot build refuses a uid that
    collides with `libs/passwd`'s static numbering.
20. **Snapshots are the unit of upgrade; the machine has a selection.** A
    snapshot is decision 8's manifest: a signed, pre-solved, monotonically
    sequenced set of package digests that CI tested together. Package files
    are trusted only through it (Debian's `Release` to `Packages` to `.deb`
    chain), so a mirror cannot substitute one.

    The **selection** is kept in `/conf` (`sys/update/selection`):
    `lazyos-base` or `lazyos-full`, options, holds, and extra or removed
    names. The installed set is the closure of the selection in the newest
    snapshot that satisfies it. `updatectl install lazyos-golf` builds a new
    *local* snapshot (the same signed digests plus the addition, a local
    sequence number, unsigned and composed only from digests a signed snapshot
    already vouched for), validates it, then stages and trial-boots it like any
    update. Consequences:
    - **Delta by package.** `updated` fetches the small index, diffs it against
      `/system/pkgdb`, downloads only packages whose digest changed, and still
      hard-links unchanged files inside a changed package (decision 9). The
      "file-level delta bundles" item of "Later" falls out of this.
    - **An offline `.lzu`** is a snapshot plus the packages a machine might
      lack; `updated` treats it as a read-only local repository.
    - **Provenance.** `/system/pkgdb/<name>.toml` in every release records the
      package's digest, files and originating snapshot, so `which`, `verify`
      and `history` are plain reads of read-only data.
    - **Holds** keep one package at its installed version; a snapshot that
      cannot honour a hold is refused and the package that forces the move is
      named.
21. **Hooks are data, applied by the owner; there are no scriptlets.**

    | Distro hook | Here |
    |---|---|
    | create system user or group | `[[user]]` fragment merged at release assembly |
    | `systemd-tmpfiles` | a unit's `state.dirs`, ensured by `init` |
    | enable and start a service | the unit exists, so it runs; `restart-units` restarts it |
    | MIME and menu database | `pkgd` already replays app manifests to `mimed` at boot |
    | font and certificate caches | the consumer rebuilds at start (`webfonts::register()`), or a `oneshot` unit under the package's own uid, idempotent |
    | data migration | the owning service at start (decision 25) |
    | kernel label and file policy | compiled from units and `[processes.*]` tables by `libs/pkgstore`'s code, loaded by `init` |
22. **The kernel is a package that proves its ABI, and learns its release from
    the loader.** A kernel shared by two snapshots is one file, so it cannot
    embed a snapshot id: the loader passes `release=<id>`, the kernel mounts
    that tree and checks its `requires.kernel-abi` against its own `provides`,
    panicking with the names on a mismatch (decision 5). A kernel-only update
    is then ordinary: a new `lazyos-kernel` whose `compat-from` still covers
    every installed service is a one-package snapshot of class `reboot`.
23. **Live switch: re-point `/system`, never mutate it.** The invariant that
    `/system` is read-only (decision 2) is kept. For snapshots whose changed
    packages are all at most `restart-units`, `updated` stages the new tree
    beside the old one and asks the kernel to **re-target the `/system`
    subtree mount** to it (`vfs_retarget`, one atomic pointer swap in
    `SubtreeFs`, allowed to `_update` only). New lookups see the new tree; open
    files and running programs keep their inodes (the old release stays until
    pruned, which hard links make cheap). Then `init` restarts the affected
    units in order. Rules:
    - Allowed only when `kernel-abi` and `requires_loader` are unchanged.
    - Until the restarts finish there is a *mixed-generation window* (new
      `netd` against old `confd`); decision 18's rules are what make it safe,
      and the harness runs it on purpose.
    - Failure: `updated` swaps back and restarts the same units. `bootctl`
      records the live release as `next` in the same trial states, so a reboot
      at any point lands on a bootable release.
    - It is the same mechanism as a rebooting update, minus the reboot.
24. **Reproducible builds are a hard requirement.** If two builds of an
    unchanged crate differ, every package changes every snapshot and deltas are
    zero. So: the pinned toolchain (`rust-toolchain.toml`) in the official
    build container, `--remap-path-prefix`, a fixed `SOURCE_DATE_EPOCH`, locked
    dependencies, no timestamps or hostnames in binaries, no snapshot id inside
    any package (decision 22), sorted archives (the writer already does this).
    `tools/pkg/repro_check.py` builds twice from different directories at
    different times and compares package digests; CI fails on a difference.
    Developer builds on Windows are not expected to match; only the Linux
    container's output is signed.

    Static linking becomes an advantage: each package records its **build
    closure** (resolved crate versions and the digests of the in-tree libs it
    links), so "what does this security fix rebuild?" is a query and CI tests
    only the affected packages.
25. **A package that owns state promises to be readable one release back.**
    Trial boots fall back, and the old release must still run on what the new
    one touched. `/conf` is covered (decision 11). Everything else a service
    owns outside it is not: `/accounts/db`, `/logs`, `keyd`'s sealed files,
    `pkgd`'s records, `/home`. So each such package declares `[state] schema =
    N` and obeys **expand/contract**: release N may write schema N+1 fields
    only if N ignores them, and the first release that *requires* N+1 ships
    only after N has been the oldest supported fallback for one snapshot. The
    owner migrates itself at start, idempotently, before serving. A checked-in
    golden of each state file, written by the previous snapshot's service, must
    still load: a build failure otherwise.
26. **`updated` and `updatectl` grow the package surface.** `idl/update.midl`
    (decision 10) adds `ListPackages`, `Show`, `Which`, `Verify`, `History`,
    `Install(packages)`, `Remove(packages)`, `Hold`, `NeedsRestart`;
    `Install`, `Remove`, `Hold`, `Rollback` and `Restart` are `elevd` actions
    (`update.install`, `update.remove`, ...) while reads need nothing.
    `updatectl` is the system package manager of guide step 7; `pkgctl` stays
    apps-only. Settings > Updates gains a *Software* page (packages, versions,
    what an update changes and costs). AGENTS.md's launcher rule applies: a
    build switch (`LAZYOS_PACKAGED=1`, later the default), a `run_demo.py` flag,
    and the Advanced-tab control through `tools/lazygui/catalog.py` with
    `test_catalog.py`.
27. **Third parties and development.** Apps stay `.lzp`: `pkgctl`, sandboxed,
    per user (U5 adds rollback and optional per-app feeds). A system package
    from outside the project is refused; a user-controlled signing key for
    system packages, if ever wanted, is a deliberate, loud, per-machine action
    in the installer, not a setting. `cargo build` keeps producing the in-place
    `dev` release (decision 1). On an image that trusts a dev key,
    `updatectl install --dev ./netd.lzs` makes a local dev snapshot with that
    package swapped, switches live and restarts the unit, and rolls back the
    same way; it replaces and generalises `dbgctl reload` and `app-install`.

## Architecture

```
 source tree: each package has a package.toml (files, units, state, activation)
   cargo (pinned toolchain, remapped paths) ─► tools/pkg/sys_build.py ─► lazyos-netd-0.4.0-3.lzs
        derives provides/requires (idl/manifest.json, ABI table) and the build closure (Cargo.lock)

 release CI: solve (every file owned once, ABI and IDL contracts, state goldens, repro_check)
   ─► snapshot-0.4.0-17.json (package digests) ─► sign ─► repo/ per channel (HTTPS),
                                                          or lazyos-<id>.lzu (snapshot + packages)

 updated (LazyOS)                      lazyos-install update (Linux)
   file │ feed │ push                    stick
     └──────────────┬─────────────────────┘
          libs/lzupdate: verify snapshot signature ─► diff vs /system/pkgdb ─► fetch changed .lzs only
          ─► stage /releases/<id>.partial (link unchanged files, extract changed)
          ─► read back ─► rename ─► bootctl: next=<id> trial tries=3
   activation = max(class over changed packages)
     reboot          ─► updatectl restart ─► grubenv lazyos_boots=4 ─► init.Shutdown(reboot)
     restart-units / next-launch / none ─► vfs_retarget(/system) ─► init restarts units ─► health ─► good | swap back

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
| **Later** | background download, an install-at-a-time setting, a watchdog on the box, a boot-time repair of an unclean volume, a Multiboot2 GRUB entry that loads the kernel from ext2 directly (grub-install-plan "Later") | as each lands |

**Order on the box:** U2a first (one evening, it decides decision 6); U0
and U2 together make the first `lazyos-install update` from Linux the last
mandatory one; U1 and U3 give updates from a file
with fallback; U1b gives the full suite and suite changes; U4 gives one click; U5 is independent and can go first.

### Package track (revision 3)

These phases make the units of U1-U4 packages instead of one tree. P0-P2 do not
touch boot and run beside U0-U2; P3 joins them to U1/U3, so **U1's `lzupdate`
and the bundle manifest are built package-aware from the start** (decision 8),
not as a file-only version rewritten later. Standing rules as above: kernel work
ships correctness and stress tests, libraries ship `cargo test` and seeded fuzz
shared with `fuzz/`, new interfaces are `.midl`, paths come from `libs/fhs`,
markers go through `trace` (a `PKG` channel), files stay under 500 lines.

| Phase | Deliverable | Evidence |
|---|---|---|
| **P0** Inventory and contracts (no behaviour change) | a `package.toml` for everything in the image; every file of a built image maps to exactly one package (`tools/pkg/owners.py`); `kernel-abi` in `libs/lazyos-sys` and the frozen-syscall table; generated `provides`/`requires` from `idl/manifest.json`; the interface-hash finding settled (decision 18); `repro_check.py` and the list of what is not yet reproducible; build-closure extraction | `owners.py` fails on an unowned or doubly-owned file in the CI image; `abi_freeze` fails when a syscall changes; the repro diff list shrinks to empty for services and data (kernel last) in CI |
| **P1** Format and assembled image | `libs/lzsys` (manifest, snapshot, validator, container via `lazypkg`), `tools/pkg/sys_build.py`; `os_compose` builds the image **from** `.lzs` files (`cargo build` still works: it runs the packager, and the `dev` release is assembled from the same packages); `/system/pkgdb`; `/system/.image-manifest` retired outright | `cargo test -p lzsys` and seeded fuzz (unknown field, path escape, zip64, setuid, doubly-owned file, bad closure); the assembled tree is byte-identical to today's for the same switches (host test over `osread tree`); `cargo test -p build-support-tests`; a boot and session show the same desktop |
| **P2** Declarative services | `/system/units/*.toml`, `[[user]]` fragments, `state.dirs`; `init` reads units instead of `MANIFEST` and `service_creds`, with a compiled-in list for the rescue path only; unit validation (closed capability list, uid collisions, dependency cycles) at snapshot build | `cargo test -p svcpolicy` (units against the old table while both exist); `init` selftest; `tools/crash/run.py`, `tools/shutdown/run.py`, `tools/accounts/run.py` unchanged and green; a unit asking for an extra capability is refused by the build |
| **P3** Packages in the update path (with U1/U3) | `updated` stages from a snapshot and fetches by package; `pkgdb` queries; `updatectl list|show|search|which|verify|history|install|remove|hold|needs-restart`; local snapshots; loader-supplied release id; the Software page | `cargo test -p lzupdate` (diff, hold, closure on add and remove, an unsatisfiable hold names the culprit); harness: an update with one changed package downloads only it; install and remove an optional package; a power cut at every staging write leaves the running release bootable; account attack harness: the session user cannot install or remove without the prompt |
| **P4** Repository and data packages (with U4) | the feed serves snapshot and packages; CA bundle, fonts, docs, firmware and wallpapers ship as independent `none`-class packages, the first thing updated with no restart; `tools/update/push.py` pushes one package | local HTTPS repo under the test CA: a partial download resumes, a wrong digest and a stale snapshot are refused; a CA update changes `curl`'s result with no restart |
| **P5** Live activation | `vfs_retarget` (kernel and `SubtreeFs`), `init.Reload` and unit restarts in dependency order, classes enforced, swap-back, `--dev` installs | kernel tests: retarget under open files, concurrent lookups, a 10^5-cycle retarget soak, `EROFS` everywhere else; harness: update `netd` and a driver during a download and audio playback, a mixed-generation window against a deliberately older `confd`, a forced failure swaps back, a too-cheap class is caught; the IDL conformance suite runs previous-snapshot clients against new providers and the reverse |
| **P6** Kernel and boot packages (with U2) | kernel-only snapshots under the ABI window; `lazyos-boot` requirement check; the rescue-kernel pin; `lazyos-install update` stages loader and rescue kernel from the same snapshot | OVMF/GRUB harness: a new kernel with older services boots, a failing trial falls back; an ABI-incompatible pairing is refused by the build, by `updated` and by the kernel, in that order |
| **P7** Retire the old paths | delete the in-place `os_update.rs` paths, the `LAZYOS_*_embed.rs` modules that duplicate package contents and the `MANIFEST` const; suites compile to meta-packages; launcher, CLI and GUI wiring finished; `docs/architecture/`, `build-switches.md`, `packages.md` and AGENTS.md rewritten (no aliases, no deprecation stage) | no reference to `.image-manifest` remains; `check_literals.py`, `check_file_length.py`, `check_adhoc.py --ratchet` green |

#### P0 spike results (2026-10-21)

Three questions the plan rested on, measured on the Windows dev host with the
pinned nightly (`nightly-2026-09-25`). Scratch builds only; nothing in the
image changed.

**S1. Is the bind-time interface hash a compatibility check? No, and the IDL
discipline is better than feared.** The id is `fnv1a64(name)`
(`midlc_model.Interface.id`), so appending a method or field leaves it alone
and removing one leaves it alone too. `tools/pkg/idl_compat.py` (new, with
`test_idl_compat.py`) diffs two IDL trees for non-append-only changes
(removed or retyped fields, changed method ids, reordered enums, removed
interfaces, changed topics). Run over all 82 commits that touched
`idl/*.midl` (3 minutes; commits in an older syntax today's `midlc` refuses are
skipped and counted), it found **2 commits with a break inside one `.vN`**: the
`Tick` method moving to a topic in `de4831856` and the `regd` to `confd` rename
in `d7fa3864a`. Both are early renames, not drift in a live interface. Field
ids are positional unless pinned, but every field in `idl/` is already pinned
and `test_midlc_fields.py` fails on an unpinned one, so a reorder cannot
renumber the wire unnoticed.

**S2. Are builds reproducible? Yes for what was measured, with two leaks.**

| Experiment | Result |
|---|---|
| 64 `user` binaries, release, rebuilt into a fresh target dir | all 64 bit-identical |
| the same, from a second checkout at a different path, a minute later | all 64 bit-identical |
| kernel, release profile, two checkouts at different paths | bit-identical (5.1 MB) |
| kernel, default dev profile (`debug = line-tables-only`) | **differs**: debug info carries absolute checkout paths |
| absolute paths left in the release kernel | 4: three `.cargo\registry\src\...` crate paths and the `.rustup` toolchain path (panic locations) |
| release kernel with `--remap-path-prefix` for `.cargo` and `.rustup` | no absolute path left |

So local crates already embed relative paths and nothing embeds a timestamp,
which is the hard part. The leaks are the cargo and rustup homes (identical on
one machine, different in a CI container, so official builds pin them or
remap) and the dev kernel's debug info (dev builds are not signed anyway).
Remapping needs `CARGO_ENCODED_RUSTFLAGS`, because setting `RUSTFLAGS`
replaces `.cargo/config.toml`'s `--cfg aes_force_soft`. **Not measured, and
still open for P0:** the static-musl xui apps and their Blitz/WebKit
dependencies, the zig-built C programs (dash, sqlite3, jq, Doom, Quake),
Docker-built WebKit, a Windows-versus-Linux comparison, and the core `.lzp`
archives against their inputs. Builds are fast (all 64 services in 27 s, the
kernel in 45 s), so a CI `repro_check.py` is cheap.

**S3. Can every file be given one owner? The seam exists.** Every file of the
image goes through one trait, `Sink::add_placed` (`build_support/os_image.rs`),
called from about twenty producer modules (`*_embed.rs`, `drivers.rs`,
`core_packages.rs`, `docs_embed.rs`, `samples_embed.rs`, ...). Today it is a
`BTreeMap` insert where **a later entry for the same path silently replaces
the earlier one**, so a double owner cannot be detected, and `/system/.image-manifest` is the
only record. Wrapping the sink to tag each file with its producer gives the
ownership table with no change to the producers, and those twenty modules are
a natural first cut of the package list (a producer is a package or a group
of them). The default console image has only 16 programs in `/system/bin`
(`init`, `messengerd`, `keyd`, `logd`, `healthd`, `clipboardd`, test probes),
217 doc files and a handful of data files; the full service set exists only
in the services and desktop images. A package set is therefore a function of
the build switches, which the snapshot build must enumerate per suite, not one
fixed list.

**Consequences for the phases.** P0's three open questions are answered, so
P0 shrinks to the list below. Nothing found argues for changing the package
model.

**P0 status (2026-10-21).**

| Item | State |
|---|---|
| IDL compatibility check | done: `tools/pkg/idl_compat.py` and tests, run by `midlc.yml` on pull requests against the base branch. It is informational (`continue-on-error`, with a job summary): there is no release tag yet and an early deliberate break inside a `.vN` is still allowed. Making it a gate is removing one line once the first snapshot exists |
| Pin field ids | already true and already enforced (see S1); nothing to do |
| Owner-tagging sink | done: `build_support/os_owners.rs`; `OsFiles::produced_by(name, ...)` scopes each producer in `build.rs`; `target/os-owners.tsv` lists `path<TAB>owner` on every build; a path placed by two producers is a warning, and `LAZYOS_OWNER_STRICT=1` (set in `ci.yml` and `image.yml`) makes it a build error. Tests: `build_support/tests/owners_tests.rs`. Measured on the console, services and two desktop images (231 to 317 files): no collisions. Inline `files.add_file(...)` lines in `build.rs` are owned by `build.rs` until P1 gives each service its own package |
| `repro_check.py` | done: `tools/pkg/repro_check.py` and tests build a revision in two checkouts with different path lengths, with the cargo home, toolchain and checkout paths remapped, and compare. Result: 64/64 `user` programs and the kernel bit-identical. `repro.yml` runs it weekly, on dispatch and when the toolchain or the checker changes. Still unmeasured: musl xui apps, zig C programs, Docker WebKit, Linux versus Windows, core `.lzp` archives |
| Official-build remap flags | the checker applies them; the official build script that signs a snapshot does not exist yet (P1) |
| `kernel-abi` and the frozen-syscall table | done: `lazyos_sys::abi::{KERNEL_ABI, KERNEL_ABI_COMPAT_FROM}` (both 1) and `libs/lazyos-sys/abi/syscalls.abi`, 79 entries (35 native syscalls, 29 Messenger ops, 15 device ops). `tests/abi_freeze.rs` fails on a renumbered or renamed constant, on a new constant missing from the table, on a vanished entry without a `removed` mark, and when `KERNEL_ABI` is not the newest `since`/`removed` in the table. It runs in CI with the rest of `lazyos-sys` and complements `kernel_tables.rs`, which ties the numbers to the kernel's dispatch. The kernel does not export the value at run time yet (P6) |
| Package list and `owners.py` | done as a first cut: `packaging/packages.toml` lists 40 packages (the kernel and boot files included) by exact path, pattern or producer; `tools/pkg/owners.py` joins it with `target/os-owners.tsv` and fails on an unowned or ambiguous file; `image.yml` runs it on the fresh image. The union of four image variants (326 files) is fully owned; the one unowned file it found (`pkgdemo.lzp`) is fixed. Optional-app producers that need external artifacts (WebKit, Doom, Quake, ...) are covered by producer only and unverified until a CI image builds them. Per-package `package.toml` files next to the code, and splitting `lazyos-core-apps` and `lazyos-assets`, are P1 |

P0 is done apart from the unmeasured reproducibility cases and the producers
above, both of which only widen coverage of checks that already exist.

**Package track order:** P0 first and alone, because it is where the plan can
turn out wrong (interface hashes, reproducibility, the number of unowned
files). Then P1 and P2 in parallel with U0-U2, P3 as U1/U3 land, P4 with U4
(the first user-visible win), P5 optional for the first package-based release,
then P6 and P7.

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
   is why it reuses the WebKit build volume and why package-level fetching
   (decision 20) matters most for this suite. A machine on a slow link can
   stay on `base`.
10. **What belongs in `full`.** `tools/test_suites.py` makes every new app
    switch choose `full`, an option or test-only, so the suite cannot drift
    from the app list silently; the choice itself is a review question.

Revision 3 adds:

11. **Reproducibility may be expensive.** Rust embeds paths and sometimes
    hashes, and the kernel is built through an artifact dependency. A package
    that cannot be made bit-identical changes every snapshot and only it pays
    (acceptable), but P0 must measure before deltas are promised.
12. **Static linking and security fixes.** A fix in `libs/crypto` changes every
    package linking it. Closure metadata and package fetching limit the
    download, not the rebuild: a fix to a widely linked library is a large but
    cheap-to-install snapshot.
13. **Contract discipline.** Append-only MIDL and syscall rules exist; nothing
    yet *tests* them across snapshots. P5's conformance run (previous clients
    against new providers and the reverse, `idl/conformance`) is the test, and
    a package whose class is cheaper than its changes allow is refused.
14. **Mixed-generation windows** (P5) are the least-explored part: a wrong
    restart order, a client that caches a binding across a provider restart.
    Live activation stays opt-in per package until the conformance run is
    clean, and `reboot` stays the safe default class. If it never gets there,
    dropping P5 loses only the no-reboot updates; the package model, deltas,
    `which`/`verify`, holds and rollback still pay for themselves.
15. **State outside `/conf`.** Expand/contract (decision 25) is a convention.
    The accounts database and `keyd`'s sealed files are the dangerous ones;
    each gets a golden-file test before its package may be anything but
    `reboot`.
16. **Bootstrap set size.** If `init`, `messengerd` and `confd` grow, more
    updates are `reboot`-class. `updatectl needs-restart` over the last N
    snapshots shows how often each class was needed; the split is revisable.
17. **Two manifest formats.** `.lzs` and `.lzp` share a container, not a schema
    (decision 16); the cost is two parsers, the benefit is that no app can
    ever be read as a system package.
18. **Granularity is a guess.** About sixty packages is a starting point;
    merging or splitting is a metadata change as long as P0's ownership check
    stays green. "No file with two owners" is not negotiable.
