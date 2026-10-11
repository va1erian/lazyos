# Plan: netbooting LazyOS from a home registry

> **Status: draft, revision 2 (2026-10-11). Nothing here is built.**
> Revision 2 records two decisions: the Kaby Lake box runs **LazyOS only**
> (no Linux, no GRUB, the firmware boots straight into our loader), and the
> boot stage is to become **a stripped-down LazyOS that boots over the box's
> own Wi-Fi** once Wi-Fi exists; §8 is the feasibility study and the design
> of that stage, and iPXE over the cable is the first step towards it, not the
> destination.
> Target: the Kaby Lake box ([`kabylake-box-plan.md`](kabylake-box-plan.md),
> [`compat/kabylake/boot-1/NOTES.md`](compat/kabylake/boot-1/NOTES.md): UEFI,
> Secure Boot off, a GPT SATA SSD, an RTL8111H wired port that LazyOS already
> drives on bare metal, an RTL8822CE Wi-Fi card, 8 GiB) and the dev machine
> plus homelab machine on the same home network. Builds on
> [`usb-stick.md`](usb-stick.md) (the RAM root and the loader files),
> [`real-pc-boot-plan.md`](real-pc-boot-plan.md) H0 (why the root is a ramdisk
> on a real PC), [`nvme-install-plan.md`](nvme-install-plan.md) and
> [`grub-install-plan.md`](grub-install-plan.md) (GPT, AHCI on the box, the
> home partition), [`update-plan.md`](update-plan.md) (the state volume, the
> slot-aware loader, signed bundles) and the Wi-Fi plans
> ([`wifi-plan.md`](wifi-plan.md), [`wifi-prerequisites-plan.md`](wifi-prerequisites-plan.md)).
> Facts read from the repository are stated plainly; facts about iPXE and
> firmware that this repository has never exercised are marked *to confirm*
> and settled in Q0.

## 1. Short answer

1. **Yes, a stripped-down LazyOS can be the boot OS, and it is the only way
   to netboot over Wi-Fi.** Neither any PC firmware nor iPXE drives a modern
   Wi-Fi card (iPXE's wireless drivers stop at 2010-era Atheros and Realtek
   parts), so "netboot over Wi-Fi" means a LazyOS kernel is up, with
   `wlanmd`, `netd` and a NIC driver, before the system image is fetched.
   What that stage lacks today is one kernel feature, **kexec**: load a
   downloaded kernel and ramdisk and start them exactly as the `bootloader`
   UEFI stage would. Everything else it needs (a NIC driver, DHCP, HTTP, a
   window to pick in, a USB keyboard) exists. §8 sizes it: the kexec is a
   few hundred lines of glue over the `bootloader-x86_64-common` crate the
   loader itself is built from, plus a device-quiescing step, plus a boot
   profile of about a dozen services in a ramdisk of a few tens of MiB.
2. **Build it in two steps, and use the cable until Wi-Fi exists.** Step A is
   iPXE on the box's ESP chaining to the registry over the wired port: no OS
   code, one evening to prove, and it gives the build-push-reboot loop at
   once. Step B is the native boot OS, developed and proven **over the same
   cable** (the RTL8168 driver already works on the box) with kexec as its
   one kernel feature; it replaces `ipxe.efi` on the ESP and keeps the same
   registry, payload and push. Step C is nothing but a Wi-Fi interface
   appearing under `netd` (WP1 already gives wireless interfaces their own
   metric), so the boot OS picks the registry up over the radio with no
   change to the picker. The registry and payload are designed once (§5, §6)
   so that A, B and C are three front ends of one system.
3. **What travels is exactly what the USB stick carries today**: the loader,
   `kernel-x86_64`, `boot.json` and the `ramdisk` (104 MiB for the dev
   desktop, about 130 MiB on the box). The kernel already treats a ramdisk as
   the root (`FS:ROOT:ram0p2`), so **step A needs no kernel change**. The
   system is RAM; the box's SSD holds only what is the user's.
4. **The registry is a small Python HTTP server on the homelab** (standard
   library only, like every harness server in `tools/net/`): immutable builds
   under channels, a generated iPXE menu *and* a JSON listing for the native
   picker, a push API with a token, per-machine defaults and a one-shot
   "boot this next". `LAZYOS_NETBOOT=1 cargo build` writes the payload and a
   manifest to `target/netboot/`; `python tools/netboot/push.py` uploads it.
   Both front ends get the switch (`run_demo.py --netboot [--push URL]`, the
   launcher's Advanced tab), as AGENTS.md requires.
5. **The box is LazyOS only, so the ESP is shaped like the stick's FAT.** The
   firmware boots `\EFI\BOOT\BOOTX64.EFI` with no menu in between. In step A
   that file is `ipxe.efi` and a rescue LazyOS (loader, kernel, ramdisk at
   the FAT root, as on the stick) is the second firmware entry iPXE falls
   through to when the registry is unreachable. In step B the boot OS *is*
   the ESP's LazyOS: its picker lists the registry's builds and "the local
   copy", and kexecs either, so the rescue system and the boot OS are one
   image.
6. **The user's partition stays on the box.** `/home` is mounted by label as
   the stick does (`home=LABEL=lazyhome`), from a `lazyhome` partition on the
   SSD as soon as the GPT reader and the AHCI verification land (the GRUB
   install plan's K1/K2, shared), and from the LazyOS stick left plugged in
   until then. The "real installation feel" (settings, installed apps and
   logs that survive a reboot) is the update plan's state volume (U0), which
   netboot is the first real consumer of: N6 here is that work, shared.
7. **The loop becomes: build, push, reboot the box, watch.** `push.py
   --machine box --reboot` marks the build as the box's next boot and asks
   `dbgd` to reboot it (one new control-tier method that asks `init`); the box
   comes up on the new build with no keyboard touched, `dbgctl.py log --follow`
   shows it, and the stick is never rewritten again.

## 2. The boot chain

```
 dev machine                      homelab (Wi-Fi)                   the box
 LAZYOS_NETBOOT=1 cargo build     tools/netboot/registry.py         UEFI firmware ─► ESP \EFI\BOOT\BOOTX64.EFI
   target/netboot/                  /srv/lazyos/<channel>/<id>/      step A: ipxe.efi + autoexec.ipxe (wired)
     bootx64.efi                      bootx64.efi kernel-x86_64        └─ GET /boot.ipxe?mac=.. ─► menu ─► imgfetch x3 ─► chain bootx64.efi
     kernel-x86_64                    boot.json ramdisk manifest.json    fallback: exit ─► 2nd firmware entry: rescue LazyOS on the ESP
     boot.json                      GET /boot.ipxe        (iPXE menu)   step B: the boot OS (the ESP's own LazyOS, wired)
     ramdisk                        GET /api/builds       (JSON)         └─ kernel ─► init ─► netd ─► netpicker: GET /api/builds
     manifest.json                  GET /files/<ch>/<id>/<name>             ─► fetch kernel + ramdisk to RAM ─► init.Shutdown(kexec)
 python tools/netboot/push.py  ───► PUT /api/builds/.. (token)              ─► the system kernel: ram0 = /, /home = LABEL=lazyhome
                                    POST /api/machines/<name>/next       step C: step B with wlanmd and the RTL8822CE (or a dongle)
```

## 3. What exists, what is missing

| Piece | State today | Needed |
|---|---|---|
| Loader files | `build_support/usb_image.rs` writes the UEFI stage (`efi/boot/bootx64.efi`), `kernel-x86_64`, `boot.json` and the `ramdisk` into the stick's FAT; `tools/boot/preflight.py` re-reads them | the same four files as loose files in `target/netboot/` plus a `manifest.json` (N1) |
| Loader load path | `vendor/bootloader/uefi/src/main.rs`: `Disk` mode opens the Simple File System of the device it was loaded from and reads the three names from its root; `Tftp` mode (upstream's) when `Disk` fails | nothing for step A if iPXE's virtual volume satisfies `Disk` mode (Q0); else one small patch (§4, decision 2). Step B does not use the loader for the second kernel at all |
| RAM root | `ram0` wins root selection when its boot volume carries `lazyos.cfg`; `FS:ROOT:ram0p2`; `/home` by `LABEL=lazyhome` found at boot or late through `usbd` | nothing for step A |
| Build identity | `LAZYOS_REVISION` in `uname` and Settings -> About (#754) | an `image.id=` line in `lazyos.cfg`, logged as `BOOT:IMAGE:<id>` and shown in About (N1) |
| Starting a second kernel | none; `power` (syscall 21) knows off and reboot, only `init` may call it ([`shutdown.md`](shutdown.md)) | **kexec** (B1, §8.2): a `power` kind that `init` reaches after the ordinary shutdown order, so every driver has stopped its device |
| The box's network | RTL8168 `netdrv` on bare metal: DHCP, ping, HTTP (boot-1 notes); `fetch` over `netd`; `dbgd` read-only inspection and the control tier | a `power.reboot` control method that asks `init` (N5); for step C a Wi-Fi driver and `wlanmd` (the Wi-Fi plans; the box's RTL8822CE is `rtw88` in Linux, dual GPL-2.0 OR BSD-3-Clause *to confirm per file*, so extractable under wifi-plan §4's rule) |
| The box's disk | AHCI driver written, not run on the box (ahci-plan A4); GPT refused (`block/partition.rs` logs the protective MBR) | grub-install-plan K1/K2, not duplicated here |
| State | `/apps`, `/conf`, `/logs` on the root, which is RAM on a stick or netboot; `pkgd` unpacks every core package at every boot | update-plan U0: a `state` volume as `/`, the OS volume read-only at `/system` (N6) |
| Writing the ESP from LazyOS | FAT is read-only in the kernel, FAT32 refused (#248); nothing in user space writes FAT | step B's boot OS must be updatable from the running system: the ESP is FAT16 and the boot OS's files are written by a small FAT16 writer, or the boot OS lives in an ext2 slot the update plan's shim reads (§8.4) |
| Registry, push, menu, picker | nothing; `tools/net/*peers.py` are the model for a stdlib host server; `tools/xui/new_app.py` writes a xui app | `tools/netboot/registry.py`, `push.py`, `install_box.py`, `run.py`, tests (N2 to N4); `netpicker` (B2) |
| Harness | `tools/boot/run.py` boots OVMF from `usb-storage` only, judges markers, pixels and `PKGD:PROVISION:DONE` | `tools/netboot/run.py`: OVMF, no disk, PXE to `ipxe.efi` or the boot OS on an ESP image, the registry on the host (N3, B4) |

## 4. Decisions

1. **iPXE first, the native boot OS second, both against one registry.**
   Alternatives weighed for the first stage: (a) the firmware's own UEFI HTTP
   Boot (one file from a URL that DHCP or the setup screen provides; no menu,
   no Wi-Fi); (b) firmware PXE straight into the vendored loader's `Tftp`
   mode (needs a proxy-DHCP on the LAN, offers no choice, and TFTP is a
   lockstep protocol whose throughput collapses over a Wi-Fi hop); (c) the
   native boot OS from day one (blocked on kexec and, for Wi-Fi, on the
   whole Wi-Fi track). iPXE costs one file on the ESP and no OS code, so it
   is step A; it is GPLv2 and shipped as a binary we neither link nor modify
   (`THIRD_PARTY.md` gets its row). The native boot OS is step B because it is
   the only route to Wi-Fi (short answer 1) and because a LazyOS picker, with
   LazyOS's own drivers and UI, is what the project wants its boot stage to be.
2. **Step A hands over loose files through iPXE's virtual volume.** iPXE
   exposes every image it has fetched (`imgfetch --name <name> <url>`) as
   files on a virtual filesystem that it installs on the device handle of the
   EFI program it chainloads; this is how it boots the Linux EFI stub and
   `wimboot` *to confirm for an arbitrary EFI application*. The vendored
   loader's `Disk` mode asks exactly that handle for a Simple File System and
   opens `kernel-x86_64`, `boot.json` and `ramdisk` at its root. Q0 proves it
   under OVMF. Fallbacks in order: `sanboot` of an ESP-only disk image over
   HTTP (*to confirm*; doubles the RAM footprint); a small patch to the
   vendored UEFI stage, kept beside the interrupt-disable patch already there.
3. **The payload is the stick's payload.** One file table, one ramdisk writer
   (`usb_ramdisk::write`), one loader: `LAZYOS_NETBOOT=1` adds an output
   directory, not a second image format; a build with both switches writes
   identical bytes to the stick and to `target/netboot/`. The same profile
   rule holds (`usb_stick::check_profile`: `LAZYOS_USB=1` and a services or
   desktop session, because the box has no PS/2 port). The native picker
   fetches the same `kernel-x86_64` and `ramdisk`; it ignores `bootx64.efi`
   and `boot.json` (it is the loader).
4. **Builds are immutable and named; channels order them.** A build id is
   `<yyyymmdd-HHMMSS>-<rev8>[-dirty]`; a pushed id is never overwritten (the
   same id with other digests is refused). A channel (`dev`, `box`, `stable`)
   is an ordered list of builds, newest first. Pruning keeps the last N per
   channel and every pinned build. The registry stores files and JSON and
   generates text; it never reads a byte of the ramdisk.
5. **Trust is the LAN's, as it is for a stick, and it is written down.**
   Plain HTTP, no signatures in v1: anyone on the home network can serve a
   kernel to the box, exactly as anyone who can plug a stick into it can.
   The registry binds to the LAN address, refuses pushes without the bearer
   token from its token file, refuses `..` and absolute paths, logs every
   request. Right for a development box on a home network, wrong for
   anything else; §11 names the signed path.
6. **Home by label now, UUID later.** v1 keeps `home=LABEL=lazyhome`; the
   SSD's home partition is labelled `lazyhome`. `home=UUID=` comes with
   per-machine configuration (§11).
7. **The box is LazyOS only, so nothing of ours depends on another OS.** The
   ESP is ours, FAT16 (the kernel reads FAT12/16; the NVMe plan already
   chooses FAT16 for the same reason), laid out like the stick's FAT so the
   rescue system is simply "the stick, on the SSD". The first install of the
   ESP and the partitions is done from a stick boot (the NVMe plan's N3
   installer, with the GRUB plan's `lazyos-install` core) or, this once, from
   a live Linux stick with `install_box.py`; after that every update of the
   boot stage comes from the running LazyOS (§8.4).
8. **Evidence first.** `BOOT:IMAGE:<id>` on serial and in About; `BOOT:VIA:
   kexec` when a boot OS started the kernel; the registry logs which machine
   fetched which build; `push.py` prints the id it pushed and `run.py`
   requires the kernel to report the same id. CI runs both stages under OVMF
   with a host-side registry.
9. **The boot OS is a build profile of the one kernel, not a second OS.**
   `LAZYOS_BOOTOS=1` is a profile like `LAZYOS_DESKTOP=1` (§8.3): the same
   kernel, `init`, services and `xuid`, a shorter manifest and one app. The
   project does not grow a second code base to boot the first.

## 5. The registry

`python tools/netboot/registry.py --root /srv/lazyos --bind 192.168.1.5:8420
--token-file /etc/lazyos-registry.token` on the homelab, with a sample
systemd unit (`tools/netboot/lazyos-registry.service`). Standard library only
(`http.server`, `json`, `hashlib`); it runs on any Linux or Windows machine.

```
/srv/lazyos/
  builds/<channel>/<id>/{bootx64.efi,kernel-x86_64,boot.json,ramdisk,manifest.json}
  builds/<channel>/<id>.incoming/     (an upload in progress; renamed when complete and verified)
  channels.json     {"dev": {"keep": 10, "pinned": ["20261011-1932-2eeee936"]}, ...}
  machines.json     {"box": {"mac": "aa:bb:...", "channel": "dev", "timeout_ms": 5000,
                              "next": null, "last_boot": {"id": ..., "at": ...}}}
  access.log
```

| Method and path | Who | What |
|---|---|---|
| `GET /boot.ipxe?mac=${net0/mac}` | iPXE (step A) | the menu below, built for that machine: its channel first, its default build preselected, `next` consumed (one shot) and recorded in `last_boot` |
| `GET /builds/<channel>/<id>.ipxe` | iPXE | the four `imgfetch`/`chain` lines for one build |
| `GET /api/builds[/<channel>]?mac=..` | `netpicker` (step B), tools | JSON: every build's manifest, the channel order, this machine's default and `next` (consumed the same way) |
| `GET /files/<channel>/<id>/<name>` | iPXE, `netpicker` | the file, with `Content-Length`; `Range` honoured |
| `POST /api/machines/<name>/booted {"id": ...}` | `netpicker` | records `last_boot` (iPXE's fetch of `.ipxe` does the same implicitly) |
| `PUT /api/builds/<channel>/<id>/<name>`, then `POST .../commit` | `push.py`, token | files land in `<id>.incoming/`; `commit` checks every digest against the manifest and renames the directory into place, or deletes it |
| `POST /api/channels/<channel>/pin|unpin|prune` | `push.py`, token | pins, and pruning by `keep` |
| `POST /api/machines/<name>/next {"id": ...}` | `push.py`, token | the one-shot default for that machine's next boot |
| `DELETE /api/builds/<channel>/<id>` | token | refused for a pinned build |

The iPXE menu is plain script:

```
#!ipxe
set reg http://192.168.1.5:8420
menu LazyOS on ${net0/mac} (box)
item --gap -- dev
item b_20261011_1932 2026-10-11 19:32  2eeee936  desktop net dbgd usb  104 MiB
item b_20261011_1810 2026-10-11 18:10  3c2389f6  desktop net dbgd usb  104 MiB
item --gap -- stable
item b_20261003_0900 2026-10-03 09:00  v0.3      desktop usb           98 MiB
item --gap
item local  Boot the local copy (the rescue system on the ESP)
item shell  iPXE shell
choose --default b_20261011_1932 --timeout 5000 target || goto local
goto ${target}
:b_20261011_1932
chain ${reg}/builds/dev/20261011-1932-2eeee936.ipxe || goto menu
:local
exit
```

and a build's script is:

```
#!ipxe
imgfetch --name kernel-x86_64 ${reg}/files/dev/20261011-1932-2eeee936/kernel-x86_64
imgfetch --name boot.json     ${reg}/files/dev/20261011-1932-2eeee936/boot.json
imgfetch --name ramdisk       ${reg}/files/dev/20261011-1932-2eeee936/ramdisk
chain --name bootx64.efi      ${reg}/files/dev/20261011-1932-2eeee936/bootx64.efi
```

A `next` set by `push.py --machine box` makes that build the default with no
wait, once, for whichever picker asks. A machine the registry does not know
gets the `dev` channel and a 10 s menu. `access.log` has one line per request
with the client address, so "did the box fetch the new ramdisk?" is a `tail`.

## 6. The payload and the push

`LAZYOS_NETBOOT=1 cargo build` (the same inputs as `LAZYOS_USB_IMAGE=1`,
which it may be combined with) writes:

```
target/netboot/
  bootx64.efi      the vendored bootloader UEFI stage (usb_image::uefi_loader)
  kernel-x86_64    the trimmed kernel
  boot.json        the loader's config (the 1280x720 minimum mode, as the stick)
  ramdisk          usb_ramdisk::write: lazyos.cfg + the ext2 OS volume
  manifest.json    {"id", "revision", "dirty", "built_at", "host",
                    "switches": ["LAZYOS_DESKTOP", "LAZYOS_NETD", ...],
                    "files": {"ramdisk": {"size", "sha256"}, ...}}
```

The ramdisk's `lazyos.cfg` gains `image.id=<id>`; the kernel logs
`BOOT:IMAGE:<id>` after `BOOT:MEDIA` and About shows it under the revision.
`tools/boot/preflight.py` learns a `--netboot DIR` mode that applies its
existing checks (a PE32+ x86-64 `bootx64.efi`, the ramdisk's MBR and
`lazyos.cfg`, the ext2 label) to the directory.

```bash
python tools/netboot/push.py                       # target/netboot -> $LAZYOS_REGISTRY, channel dev
python tools/netboot/push.py --channel box --machine box --reboot --follow
python tools/netboot/push.py --list | --pin ID | --prune
python tools/run_demo.py --netboot --push http://192.168.1.5:8420 --build-only
```

`push.py` reads the registry URL from `--registry`, `LAZYOS_REGISTRY` or
`tools/netboot/registry.local.json` (git-ignored), the token from a file, runs
the preflight, uploads with the digests from the manifest, commits, and prints
the id. `--machine NAME` sets that machine's `next`; `--reboot` then asks the
box through `dbgd` (N5) and `--follow` tails its log until `BOOT:IMAGE:<id>`
and `SHELL:DESKTOP:PASS` arrive, or a timeout. The launcher gets a *Netboot*
group on the Advanced tab (the switch, registry URL, channel, machine, a Push
button) wired through `tools/lazygui/catalog.py` with `test_catalog.py` rows.

## 7. On the box (LazyOS only)

The SSD's layout is the NVMe install plan's GPT with `ahci0` for `nvme0`
(ESP, slot A, slot B, `lazyboot`, `state`, `home`; the stick build and this
plan only ask that the ESP be FAT16 and that `home` be labelled `lazyhome`).
Until the GPT reader and the AHCI run on the box (K1/K2) land, nothing on the
SSD is readable by LazyOS, and the box boots the stick as today; the ESP work
below can still start, because the firmware reads the ESP, not the kernel.

**Step A** (`tools/netboot/install_box.py`, run once from a live Linux stick
or from the stick-booted LazyOS once a FAT16 writer exists, §8.4): the ESP
holds the stick's FAT layout (`\EFI\BOOT\BOOTX64.EFI` = the loader,
`kernel-x86_64`, `boot.json`, `ramdisk` at the root: the **rescue system**, a
full LazyOS that needs no network) plus `\EFI\lazyos\ipxe.efi` and
`\EFI\lazyos\autoexec.ipxe`, which iPXE runs when started from a filesystem
*to confirm (Q0)*:

```
#!ipxe
dhcp || goto fail
chain http://192.168.1.5:8420/boot.ipxe?mac=${net0/mac} || goto fail
:fail
echo LazyOS registry unreachable; booting the local copy in 5 s
sleep 5
exit
```

Two firmware entries (`efibootmgr`, or the installer's own variable writes
later): "LazyOS (network)" at `\EFI\lazyos\ipxe.efi` first, "LazyOS (local)"
at `\EFI\BOOT\BOOTX64.EFI` second. `exit` in iPXE returns to the firmware,
which continues with the next entry: a dead registry costs a 5 s message,
then the local copy boots; the box is never stuck at a prompt. If the
autoexec path is not honoured, `fetch_ipxe.py --embed` builds `ipxe.efi` with
the script embedded (`make bin-x86_64-efi/ipxe.efi EMBED=...` at a pinned
iPXE revision, under WSL or Docker, as other fetched third-party code is
built). The firmware must keep **Secure Boot off** (`ipxe.efi` and our loader
are unsigned) and **Fast Boot off** (USB input), as for the stick; its own
network stack may stay off, iPXE brings its own `realtek` driver.

**Step B** replaces the two entries with one: `\EFI\BOOT\BOOTX64.EFI` loads
the boot OS (`kernel-x86_64` and a small `ramdisk` at the root), whose picker
offers the registry's builds and "the local copy"; the local copy is now a
second pair of files (`\EFI\lazyos\local\{kernel-x86_64,ramdisk}`) the picker
kexecs when the network is down, or when told to (§8).

**`/home`** is the `lazyhome` partition on the SSD once K1/K2 land (formatted
by `tools/mkdisk`'s formatter or `lazyos-install`, never `mke2fs` defaults:
the kernel driver knows only the features those write); until then, the
LazyOS stick stays plugged in and its `lazyhome` is found late through `usbd`
as today. Two `lazyhome` volumes present at once (stick and SSD) is the
ambiguity decision 6 defers: remove the stick once the SSD home exists.

**The debug loop**, from the dev machine:

```bash
python tools/run_demo.py --desktop --net --dbgd-control --netboot --build-only
python tools/netboot/push.py --machine box --reboot --follow      # BOOT:IMAGE:<id> ... SHELL:DESKTOP:PASS
python tools/dbg/dbgctl.py log --follow                           # the running box, as today
```

A broken build is one `push.py --machine box --id <previous>` (or the menu at
the box) away from the last good one; the registry keeps ten; the local copy
needs no registry at all.

## 8. The native boot OS (step B), and Wi-Fi (step C)

### 8.1 Feasibility

The question is whether a LazyOS that has booted can start *another* LazyOS
kernel from files it downloaded. Everything else in the stage is in the tree.

| The stage needs | Exists | Missing |
|---|---|---|
| A kernel that boots on the box from the ESP | yes (the stick's loader and kernel; boot-1) | nothing |
| A NIC driver, DHCP, DNS, TCP, HTTP | `netdrv` (RTL8168 on the box), `netd`, `fetch` (`nettls`, plain HTTP too) | a client library the picker uses in-process rather than spawning `fetch` (or spawn it; both fine) |
| A keyboard and a screen | `usbd`, `inputd`, `xuid`, xui | one small app, `netpicker` (`tools/xui/new_app.py`) |
| Somewhere to put 130 MiB | RAM; the kernel's shared buffers and the per-process quota (`limits.rs`) | the download lands in pages the kexec can hand to the new kernel (8.2) |
| Starting the downloaded kernel | **no** | **kexec** (8.2) |
| A Wi-Fi link (step C) | WP0-WP2 of the prerequisites (uids, `keyd` secrets, the PMK path to `wlanmd`) | the rest of the Wi-Fi track: `wlanmd`, the 802.11 station stack, a driver for the RTL8822CE or a dongle (the Wi-Fi plans own this; nothing here is specific to netboot) |
| Updating the boot OS from LazyOS | no FAT writer | 8.4 |

So the native boot OS is **feasible, and its cost is one kernel feature and
one small app**; Wi-Fi is a separate, larger track that netboot inherits
rather than drives. It is also the only way to the goal: no firmware or
third-party boot stage will ever drive the box's Wi-Fi card.

### 8.2 kexec

Design, following the rule that only `init` stops the machine
([`shutdown.md`](shutdown.md)):

- **The handover reuses the loader's own code.** The vendored UEFI stage is a
  thin shell over `bootloader-x86_64-common` (parse the kernel ELF, allocate
  and map its segments, build page tables, lay out `BootInfo`, switch). That
  crate is `no_std`, already pinned in `vendor/bootloader`, and the kernel
  can depend on it. The kexec implements the crate's `FrameAllocator` over
  the kernel's physical allocator and its mapper over `mem::layout`'s offset
  mapping, and otherwise calls the same `load_and_switch_to_kernel`. The
  `BootInfo` it builds carries the **firmware's memory map as the boot OS
  received it** (kept from its own `BootInfo` since boot), re-marked: the
  boot OS's own pages become usable (it is being discarded), the new
  kernel's frames, page tables, stack and the ramdisk are `Kernel` and
  `Bootloader` regions, the framebuffer and RSDP are passed through
  unchanged, `ramdisk_addr/len` point at the downloaded image.
- **The downloaded kernel and ramdisk are physically contiguous or
  page-listed.** The picker downloads into a shared buffer (`Buffer`, the
  shared-page object every interface may carry); `init` passes it to the
  kernel's kexec op, which pins its frames and reads them straight. The
  ramdisk needs no copy: `BootInfo` names its physical address, and
  `bootloader-x86_64-common` maps it for the new kernel exactly as it does
  today. The kernel ELF is read from the buffer by the common crate's
  loader. Memory in flight: one ramdisk, not two (the loader path copies).
- **Devices are quiet first.** `init.Shutdown(kexec)` runs the ordinary stop
  order (apps, then services in reverse dependency order, lifecycle for
  durable state, `sync`), so `netdrv` has stopped its rings, `usbd` halted
  its controllers, `sndd` its streams, and a block driver flushed and
  stood its disk by (AHCI/NVMe already do at power-off). Then `power(kexec)`
  disables interrupts, masks the I/O APIC and the LAPIC timer, and jumps.
  The new kernel initialises every device from scratch as it does after a
  firmware boot. The one state that survives is what the firmware set up
  and the kernel never changes: the GOP framebuffer, ACPI tables, the memory
  map's shape. The xHCI BIOS handoff `usbd` performs is idempotent.
- **What the new kernel sees** is a normal `bootloader` boot (`BOOT:MEDIA:
  uefi`, from the memory map's firmware types, which are preserved) plus a
  `BOOT:VIA:kexec` line from a `BootInfo` field the common crate does not
  have; the vendored crate gains one optional field, defaulting to "loader",
  which is the only patch, and it stays with the other patches in
  `vendor/bootloader`.
- **Not supported, by design:** kexec from a kernel that has woken
  application processors (S8 does not exist yet; when it does, the APs are
  parked with INIT before the jump), kexec of a BIOS-booted kernel (the boot
  OS is UEFI only), and anything but a LazyOS kernel (the op checks the ELF's
  `bootloader` config section before touching anything).
- **Tests** (AGENTS.md's two kinds): `kexec_suite` under `cfg(lazyos_tests)`
  builds a `BootInfo` from synthetic maps (UEFI shapes with holes, regions
  above 4 GiB, the ramdisk straddling a hole, a framebuffer inside a region)
  and checks every region is accounted for exactly once, that the new
  kernel's frames never overlap the ramdisk or the firmware's reserved
  regions, that a hostile ELF (segments outside RAM, overlapping, a missing
  config section, a 4 GiB `memsz`) is refused before any frame is taken; a
  soak that plans and discards 10,000 handovers over random maps and leaks
  no frame. The end-to-end proof is the harness (B4): the boot OS kexecs the
  dev image under OVMF and the dev image reaches the desktop with
  `BOOT:VIA:kexec`, then from that desktop `powerctl kexec <files>` chains a
  third time, so a leak or a device left running shows up.

### 8.3 The boot profile

`LAZYOS_BOOTOS=1` (decision 9) is a session profile: `init`'s manifest keeps
`messengerd`, `logd`, `confd` (read-only settings for the picker: the
registry URL, the machine name, the Wi-Fi network), `keyd` (the PMK, step C),
`devd`, `netdrv` (or `wlanmd` and its driver), `netd`, `usbd`, `inputd`,
`xuid` and starts **`netpicker`** as the only client, as `LAZYOS_XUI_APP`
images do today. No `pkgd`, no packages, no docs, no accounts (the picker runs
as a fixed system uid; there is nothing to log into), no sound. The ramdisk is
the kernel plus a dozen static binaries and the font: a few tens of MiB at
most against the desktop's 104 MiB (the xui apps are 0.4 MiB each; `xuid` and
the services are of that order; the fonts the compositor rasterises are the
largest item), so the firmware loads it from the ESP in well under the
stick's 5.7 s to kernel. Its `lazyos.cfg` carries `bootos.registry=`,
`bootos.machine=` and `bootos.local=` (the path pair of the local copy).

`netpicker` is one window: the channels and builds from `GET /api/builds`
(id, date, revision, switches, size), the default preselected, a countdown
that honours the registry's `timeout_ms` and `next`, a "Local copy" row and
a "Shell" row (the Terminal, for a box with no serial port), a progress bar
for the download with the bytes per second (the number step C is judged by).
Keyboard first: arrows, Enter, Escape stops the countdown; the mouse works
because `xuid` does. It fetches into a shared buffer, verifies the manifest's
SHA-256 of both files before anything else (a corrupt download is a message
and the menu again, never a boot), posts `booted`, and calls
`init.Shutdown(kexec, buffer)`. Its evidence lines are `PICKER:UP`,
`PICKER:LIST:<n builds>`, `PICKER:FETCH:<id>:<bytes>:<ms>`,
`PICKER:VERIFY:PASS|FAIL`, `PICKER:KEXEC:<id>`.

With no link (no cable, no known network) or no registry within a bounded
wait, the picker says so and counts down to the local copy, so a cold box
always boots something. "Boot the local copy" and the countdown are also
what makes the boot OS the rescue system.

### 8.4 Updating the boot OS from the running system

The boot OS changes rarely (a kexec fix, a new NIC driver, a picker feature),
but it must be updatable without a stick, since the box has no other OS. Two
routes, chosen when B3 is implemented:

1. **A FAT16 writer in user space** (`libs/fat16w`, host-tested and fuzzed
   like `lazyarc`), used by a privileged `bootosd` that holds write access to
   the ESP partition and nothing else (the update plan's "one narrow
   capability" shape): write `kernel-x86_64.new` and `ramdisk.new`, `fsync`,
   rename over the old pair. Simple and self-contained; writing FAT is a
   known amount of work; the loader keeps reading the ESP as today.
2. **The boot OS lives in an ext2 slot** and the ESP holds only the update
   plan's slot-aware loader (U2's forked UEFI stage, which reads the kernel
   out of an ext2 volume through `libs/ext2fs` and already needs to exist for
   A/B updates). LazyOS writes ext2 today; the ESP is never written after
   install. More convergence, but it waits for U2.

Either way the boot OS is itself a build the registry can carry (channel
`bootos`), and `push.py --bootos` installs it on the box through the same
`dbgd`/`init` path the system update will use. Until B3 lands, the boot OS is
refreshed by booting a stick (the stick's own loader, then `lazyos-install`
writing the ESP), which is also the recovery when a boot OS does not come up.

### 8.5 Step C: Wi-Fi

Nothing in the boot OS is specific to the radio: under WP1 a wireless card is
an interface with its own `netdrv`-shaped driver, `wlanmd` holds the
association and the keys, `netd` gives it an address and a route (metric
600, the cable at 100 wins when both are up). The boot OS adds the Wi-Fi
driver and `wlanmd` to its manifest, `keyd`'s named secrets supply the PMK
(from the `state` volume once N6 exists, else from the boot OS's own `/conf`
written at install; the ESP is plaintext either way, as Secure Boot is off),
and the picker's bounded wait grows to cover association and the 4-way
handshake. The judge gains the bytes per second over the radio.

What this plan asks of the Wi-Fi track: the station path for **one** network
with a stored PMK, robust at cold boot (the card has only ever seen the
firmware), and the box's own card or a dongle, whichever the Wi-Fi plans pick
(the RTL8822CE is a PCIe card in the box: an `rtw88`-derived driver under
the wifi-plan licence rule *to confirm*; the MT7925 dongle is the plan's
recommendation and needs `usbd` to hand a USB interface to a driver, which
the HID and storage classes already model).

### 8.6 Cost and timing against step A

| | iPXE (step A) | Boot OS (step B/C) |
|---|---|---|
| Code in the OS | none | kexec (kernel + tests), the profile, `netpicker`, later the ESP writer |
| Time to the menu on the box | 2-3 s *to measure* | the boot OS's kernel-to-picker: estimate 5-8 s (`usbd`'s 15 s for a dead USB device, boot-1 notes, is the risk; Q0 measures the stick's time to `init`) |
| Download | iPXE's HTTP, tens of MB/s on wire *to measure* | `netd` TCP; `tools/net` measures the stack's throughput, *to measure* on the box |
| Extra boot | none | the system kernel boots a second time (no firmware re-init: the kexec jump is immediate) |
| Wi-Fi | never | step C |
| UI | text menu | a LazyOS window |
| Fallback | second firmware entry | the picker's local copy |

Step B costs a longer boot (one kernel start more, perhaps 10 s) and buys
Wi-Fi, a real UI and dogfooding of the network stack on every boot. The
registry, payload and push do not move.

## 9. Phases

Each phase ends shippable. Python stays under 500 lines per file (several
small modules in `tools/netboot/`, as `tools/boot/` does); kernel work ships
correctness and soak tests under `kernel/src/tests/` and passes
`python tools/test/run.py --accel none`. Every phase that boots adds its
variant to `tools/netboot/run.py`.

| Phase | Deliverable | Evidence |
|---|---|---|
| **Q0** Feasibility (one evening, no code in the OS) | By hand under QEMU + OVMF: serve the official `ipxe.efi` over QEMU's user-network TFTP (`-netdev user,tftp=DIR,bootfile=ipxe.efi`, OVMF PXE-boots it with no disk attached), a hand-written menu and `python -m http.server` on the four files extracted from `target/lazyos-usb.img`'s FAT. Settle: does the loader's `Disk` mode read from iPXE's virtual volume (decision 2)? Is `autoexec.ipxe` beside `ipxe.efi` honoured on EFI? Does `sanboot` of an ESP image work as the fallback? Download time of the ramdisk under QEMU, then on the box with a cable (the official `ipxe.efi` written to the current stick's FAT and started from the firmware's boot menu, nothing installed); the stick's time from kernel to `init` on the box (the boot OS's floor, 8.6) | numbers and verdicts in §13; a photo of the box's first network boot in `compat/kabylake/boot-N/` |
| **N1** Payload | `build_support/netboot.rs`: `LAZYOS_NETBOOT=1` writes `target/netboot/` (decision 3) and `manifest.json`; `image.id=` in `lazyos.cfg`, `BOOT:IMAGE:<id>`, About shows it; `preflight.py --netboot`; `run_demo.py --netboot`; the launcher group and `catalog.py` | `cargo test -p build-support-tests` (digests match, the id format, a dirty tree marked, both switches write identical bytes); `mount_suite`: `image.id` parsed and logged, a bad value refused without affecting the mount; `tools/lazygui/test_catalog.py` |
| **N2** Registry and push | `tools/netboot/registry.py` (+ `store.py`, `menu.py`, `api.py`), `push.py`, `fetch_ipxe.py`, the systemd unit, `README.md` | `test_registry.py` against a loopback server: atomic commit (a torn upload leaves no build), digest mismatch refused, the same id with other bytes refused, pin and prune, token on every write, `..` and absolute names refused, `Range`, the menu and the JSON for a known machine (channel order, default, `next` consumed exactly once) and for an unknown MAC; `test_push.py` against a fake registry |
| **N3** Harness (step A) | `tools/netboot/run.py`: builds (or `--no-build`), starts the registry on a loopback port, pushes to channel `ci` with the QEMU NIC's MAC as a machine with `timeout_ms: 0`, boots OVMF with **no disk**, a virtio-net card on the user network whose TFTP serves `ipxe.efi` and whose `10.0.2.2` is the host, a USB keyboard and mouse as `tools/boot/run.py` does; judges `BOOT:MEDIA:uefi`, `BOOT:IMAGE:<the pushed id>`, `FS:ROOT:ram0p2`, `PKGD:PROVISION:DONE ... failed=0`, `SHELL:DESKTOP:PASS`, the registry log (four files, once each, by that machine) and the screenshot (`pngstats.py`, and Read the PNG). `--home-disk`: a virtio disk from `tools/mkdisk --home-volume` mounts at `/home` at boot. `--esp`: boot from an ESP image laid out as §7 with the registry *down*, and the rescue copy must come up. OVMF found as `tools/boot/run.py` finds it, plus QEMU's own `share/edk2-x86_64-code.fd` on Windows. `test_run.py` | `.github/workflows/netboot.yml` (Linux runner, OVMF, `ipxe.efi` fetched and hash-checked, artifacts like `screenshots.yml`) |
| **N4** The box, step A | `install_box.py` (§7, from a live Linux stick this once: the FAT16 ESP with the rescue copy and iPXE, two firmware entries); `docs/netboot.md` (the homelab service, the box, firmware settings, "the menu does not appear", recovery is the local copy or the stick); the `compat/hardware.md` row | the box boots the pushed build from the menu, `BOOT:IMAGE` read in About, a file saved to `/home` survives a second network boot; the registry log shows the fetches; download time recorded; the registry stopped, the box boots the local copy on its own |
| **N5** The loop | `dbgd` control method `power.reboot` (asks `init.Shutdown(reboot)`, never the kernel's `power`; refused without the control tier; `dbgwire` allowlist and tests); `push.py --machine --reboot --follow`; `machines.json` documented | `cargo test -p dbgwire`; `tools/dbg/run.py` gains the reboot (a second `BOOT:IMAGE` in the serial log); on the box: build, push, reboot, desktop, under a minute, hands off |
| **N6** State on the box (shared with update-plan U0) | `state=LABEL=lazystate`: the state volume is `/` (`/apps`, `/conf`, `/logs`), the ramdisk's OS volume read-only at `/system` (`/docs/os` to `/system/docs`, one `libs/fhs` constant), `pkgd` reconciles core packages up and down against `/system/packages/index`; `run_demo.py --state-disk` | `mount_suite`: the three-volume layout, `EROFS` under `/system`, a missing state volume falls back to the RAM root with a log line; harness `--state-disk`: a setting and an installed app survive a second network boot of a *different* build, a downgrade restores the older core packages; `tools/accounts/run.py` still blocked everywhere |
| **B1** kexec | `kernel/src/power/kexec.rs` (+ `kexec/` for the map and the ELF checks), the `power(kexec)` kind, `init.Shutdown(kexec, buffer)` in `idl/init.midl`, `powerctl kexec KERNEL RAMDISK` (for a developer, from the Terminal), the `BOOT:VIA` field in the vendored common crate | `kexec_suite` (8.2: synthetic maps, hostile ELFs, the 10,000-handover soak); `tools/test/run.py --accel none` green; harness `--kexec`: the dev image boots, `powerctl kexec` the same files from `/system/share/netboot/` (a test-only copy), the second kernel logs `BOOT:VIA:kexec` and reaches the desktop, then a third |
| **B2** The boot OS | `LAZYOS_BOOTOS=1` (8.3): manifest, `netpicker` (`tools/xui/new_app.py`, permissions from a `LAZYOS_LABEL_TRACE=1` run), the HTTP client over `netd`, `bootos.*` cfg lines; `run_demo.py --bootos`; the launcher; `target/bootos/{kernel-x86_64,ramdisk}` pushed to channel `bootos` by `push.py --bootos` | `cd xui-app && cargo test -p netpicker` (list parsing with unknown fields, a hostile manifest, the countdown, the digest check); harness `--bootos`: OVMF boots an ESP image carrying the boot OS, the picker lists the registry, fetches the pushed dev build, `PICKER:VERIFY:PASS`, `PICKER:KEXEC`, the dev build's `BOOT:VIA:kexec` and desktop; a corrupted file on the registry gives `PICKER:VERIFY:FAIL` and the menu; the registry down gives the local copy; the ramdisk size recorded in §13 |
| **B3** Install and update | `install_box.py` writes the boot OS as `\EFI\BOOT\BOOTX64.EFI`'s kernel and ramdisk and the local copy under `\EFI\lazyos\local\`; the update route of 8.4 (`bootosd` over `libs/fat16w`, or the ext2 slot with U2); `push.py --bootos --machine box` updates the box's boot OS | `cargo test -p fat16w` (host, fuzz seeds, `fsck.fat -n` clean in CI) if route 1; harness: update the boot OS from the running system, reboot, the new boot OS's `PICKER:UP` with its id; a torn update (QEMU killed mid-write) still boots one of the two |
| **B4** The box, step B | the boot OS on the box's ESP, replacing iPXE; `docs/netboot.md` updated; the compat row | kernel-to-picker and download times recorded; a week of the loop without a stick |
| **C1** Wi-Fi | the Wi-Fi driver and `wlanmd` in the `bootos` manifest, the PMK from `keyd`, the picker's wait covers association | the box with no cable boots a pushed build over the radio; bytes per second in §13; the harness cannot model Wi-Fi (QEMU has no radio), so this row is the box and `wifisim` where the Wi-Fi plans provide it |

**Order:** Q0 first and alone. N1 and N2 together; N3 next; N4 after N3 is
green in CI once (never walk to the box with a path CI has not booted); N5 is
small and makes N4 pleasant. B1 can start as soon as N1 exists and is
independent of the box; B2 needs B1 and N2; B3 and B4 follow. N6 is the
update plan's U0 and is the largest item; it can land anywhere after N3. C1
waits for the Wi-Fi track and changes nothing designed here.

## 10. Verification

Modelled on `tools/boot/run.py` and `tools/net/run.py`: the verdict is
markers, the registry's own record, and pixels.

- `python tools/netboot/run.py [--no-build] [--home-disk] [--state-disk]
  [--esp] [--sanboot] [--kexec] [--bootos] [--memory 2G] [--accel auto]`
  writes `shots/netboot/{serial.log,registry.log,screen.png,report.json}`
  (seconds from QEMU start to the menu or picker, to the kernel, to the
  desktop; bytes served; the ramdisk sizes).
- `test_registry.py`, `test_push.py`, `test_run.py`: every judge and rule
  fails when it should. `preflight.py --netboot` before every push.
- Kernel: `kexec_suite` in `python tools/test/run.py --accel none`.
- Real hardware: `docs/compat/hardware.md`'s row for the box gains "network
  boot: iPXE <s> to menu, boot OS <s> to picker, <MB/s> wired, <MB/s> Wi-Fi";
  photos of the menu, the picker and About in `compat/kabylake/boot-N/`.

## 11. Risks and open questions

1. **iPXE's virtual volume and the vendored loader** (decision 2) is step
   A's one assumption nothing here has run. Q0 settles it in an evening; the
   fallbacks are listed in order and the worst case is a 50-line loader
   patch next to the one already in `vendor/bootloader`.
2. **kexec correctness** is step B's risk: a device left doing DMA, a page
   table the new kernel does not expect, a memory map that says a region is
   free while the ramdisk sits in it. The design leans on the shutdown order
   that already quiesces drivers and on the loader's own crate for the
   handover, and the harness chains three kernels so a leak or a stray
   device shows. Expect the first bare-metal kexec to find one thing QEMU
   does not model (boot-1 found three for USB); the box's `dbgd` and the
   on-screen panic are the evidence.
3. **The boot OS's own boot time.** `usbd` spends up to 15 s on a dead USB
   device (boot-1), and the box's keyboard and mouse are full speed. If the
   picker takes longer to appear than iPXE's menu by a wide margin, the
   remedies are in `usbd` (shorter bounded retries at the boot OS's
   profile), not in this plan; Q0 measures the floor.
4. **Memory while loading.** Step A: iPXE holds the four images (about
   135 MiB) in boot-services memory and the loader copies the ramdisk into
   `LOADER_DATA` before `ExitBootServices` frees iPXE's copies. Step B: one
   copy, in a shared buffer counted against the picker's quota
   (`shared_buffer_max`, `quota_user_memory`), so the boot OS's
   `lazyos.cfg` raises those limits for its one client. Fine with 8 GiB; the
   harness runs with `--memory 2G` to keep it so.
5. **Wi-Fi throughput and the registry's placement.** 130 MiB at 20 MB/s is
   7 s, at 5 MB/s half a minute, and in step C the box's own radio is a
   second Wi-Fi hop. Levers: `LAZYOS_USB_ROOT_FREE` (the free space is
   downloaded too), a compressed ramdisk the kexec path inflates (`zstd` has
   a `no_std` decoder; the picker would inflate into the shared buffer, so
   no loader patch), and a "fast" channel of builds without docs.
6. **Updating the boot OS** (8.4) is a chicken-and-egg until B3: a boot OS
   that fails to come up is fixed by booting a stick, which the box must
   therefore keep able to do (Fast Boot off, the stick's loader known to
   the firmware). B3's torn-update test is what makes this rare.
7. **Where is `/home` while the SSD is not reachable?** On the stick, which
   means `usbd` must come up before the late home mount (it does,
   `INIT:HOME mounted`), with `usb-storage.md`'s timeouts. The SSD home
   (K1/K2) ends this; it is tracked in the GRUB install plan.
8. **An unauthenticated kernel from the LAN** (decision 5). Accepted for
   this box on this network; `docs/netboot.md` says so on its first page;
   §12 has the signed path. In step C the PMK on an unencrypted ESP or state
   volume is the same statement about the disk.
9. **`autoexec.ipxe` from the ESP** *to confirm*; the embedded-script build
   is the fallback and needs a C toolchain under WSL or Docker.
10. **The firmware and `exit`.** Step A's fallback relies on the firmware
    continuing to the next `BootOrder` entry when the first returns. If the
    box's firmware stops at its own menu instead, step A embeds the local
    boot in the iPXE script (`sanboot --no-describe --drive 0x80` or a
    `chain` of the ESP's `BOOTX64.EFI` by file path, *to confirm*), and
    step B makes the question moot.
11. **Reboot by `dbgd`** adds a remote way to stop the machine to a tool
    whose first version was read-only by design. It stays in the control
    tier (off unless built in and configured), goes through `init` like
    every other stop, and `dbgwire`'s allowlist test names it.
12. **The Wi-Fi driver's licence and the card.** The box's RTL8822CE is
    extractable only if `rtw88`'s dual licence holds for every file the
    driver needs *to confirm*; otherwise step C is the MT7925 dongle the
    Wi-Fi plan recommends, on `usbd`. Netboot does not care which.

## 12. Later

- **Signed builds.** `manifest.json` gains the update plan's U1 shape and an
  Ed25519 `manifest.sig`; the picker (and, for step A, a loader patch)
  verifies the digests of `kernel-x86_64` and `ramdisk` against a key
  compiled in, and refuses a build that does not verify. The registry serves
  the signature as a fifth file; nothing else changes.
- **Per-machine configuration** served by the registry: `machines.json`
  lines (`home=UUID=`, `display.max`, `limit.*`, `diag.hold`) patched into
  the ramdisk's 1 MiB FAT part at serve time (or, in step B, written by the
  picker before the kexec: it holds the ramdisk in RAM and the FAT part is at
  a fixed offset), so one build boots many machines with their own settings.
- **The boot OS as the A/B shim.** With the update plan's control block the
  picker can show the two installed slots beside the registry's builds and
  kexec a slot's kernel from its ext2 volume; the forked UEFI stage of U2 then
  shrinks to "load the boot OS".
- **Netbooting QEMU itself** as the default dev loop: `run_demo.py --from
  REGISTRY` boots the last pushed build without a local build, for a laptop
  that only reviews.
- **A compressed ramdisk** (risk 5) and a channel of builds without docs.
- **Firmware UEFI HTTP Boot** as a zero-file install for *other* machines
  whose firmware offers it: the DHCP names `http://registry/ipxe.efi` or the
  boot OS's loader, and the machine has nothing of ours on its disk.

## 13. Measurements

*To be filled from Q0, B2 and C1: loader verdict (virtual volume, sanboot,
patch), autoexec verdict, iPXE version and SHA-256 pinned, time to the iPXE
menu and to the picker on the box, the stick's kernel-to-`init` time, the boot
OS ramdisk size, download rates on the wire and over the radio.*
