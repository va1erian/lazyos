# Plan: netbooting LazyOS from a home registry

> **Status: draft, revision 3 (2026-10-12). Nothing here is built.**
> Revision 3 follows the merge of native Wi-Fi (#776,
> [`wifi-rtw88-plan.md`](wifi-rtw88-plan.md) §5.6: the Kaby Lake box joins a
> WPA2 network on 5 GHz with `wifid`, `wlanmd` and `netd`). The destination
> of revision 2, **a stripped-down LazyOS as the boot OS, fetching the system
> over the box's own Wi-Fi**, is now reachable with one kernel feature,
> **kexec**, and this revision makes it the path (§8). iPXE over the cable
> becomes an optional interim for a box that has a cable plugged in, not a
> step everyone takes. The box runs **LazyOS only**: the firmware boots
> straight into our loader, no other OS, no GRUB.
> Target: the Kaby Lake box ([`kabylake-box-plan.md`](kabylake-box-plan.md),
> [`compat/kabylake/boot-1/NOTES.md`](compat/kabylake/boot-1/NOTES.md): UEFI,
> Secure Boot off, a GPT SATA SSD, an RTL8822CE Wi-Fi card and an RTL8111H
> wired port, both driven natively, 8 GiB) and the dev machine plus homelab
> machine on the same home network over Wi-Fi. Builds on
> [`usb-stick.md`](usb-stick.md) (the RAM root and the loader files),
> [`real-pc-boot-plan.md`](real-pc-boot-plan.md) H0 (why the root is a ramdisk
> on a real PC), [`nvme-install-plan.md`](nvme-install-plan.md) and
> [`grub-install-plan.md`](grub-install-plan.md) (GPT, AHCI on the box, the
> home partition), [`update-plan.md`](update-plan.md) (the state volume, the
> slot-aware loader, signed bundles), [`architecture/wifi.md`](architecture/wifi.md)
> and [`shutdown.md`](shutdown.md) (only `init` stops the machine). Facts
> read from the repository are stated plainly; what this repository has
> never run is marked *to confirm* and settled by the measurements of §13.

## 1. Short answer

1. **The boot OS is LazyOS.** A boot profile of the one kernel
   (`LAZYOS_BOOTOS=1`: `init`, `confd`, `keyd`, `devd`, `wifid`, `wlanmd`,
   `netd`, `usbd`, `inputd`, `xuid` and one app, `netpicker`) boots from the
   box's ESP in a ramdisk of a few tens of MiB, joins the home network the
   way the desktop does today (a **system-scope** Wi-Fi profile, which
   `wlanmd` auto-connects before anyone logs in), lists the builds the
   homelab registry holds, downloads the chosen kernel and ramdisk into RAM,
   verifies their digests and **kexecs** them. The system then runs from RAM
   with `/home` on the box's SSD. No firmware and no third-party boot stage
   can do this: none of them drives a modern Wi-Fi card.
2. **kexec is the critical path and the only kernel work.** Start a
   downloaded `bootloader`-protocol kernel from a running LazyOS exactly as
   the UEFI stage would: build `BootInfo`, map the kernel, hand over the
   ramdisk, jump. §8.2 designs it as glue over `bootloader-x86_64-common`,
   the crate the loader itself is built from, reached through `init`'s
   shutdown order so every driver has quiesced its device. It is developed
   and proven in QEMU over virtio-net (no radio needed), then on the box.
3. **The registry, the payload and the push are the same whatever boots
   them.** A Python standard-library HTTP server, shipped as a Docker
   container for the homelab's Cosmos Cloud (one port, one volume, §5), keeps
   immutable builds under channels, a JSON listing for the picker (and an
   iPXE menu, if iPXE is ever used), a push API whose writes are Ed25519
   signed, per-machine defaults and a one-shot "boot this next".
   `LAZYOS_NETBOOT=1 cargo build` writes the stick's `kernel-x86_64` and
   `ramdisk` plus a signed `manifest.json` to `target/netboot/`;
   `python tools/netboot/push.py` uploads it. Both
   front ends get the switch (`run_demo.py --netboot [--push URL]`, the
   launcher's Advanced tab), as AGENTS.md requires.
4. **Only the system travels.** `/home` stays on the SSD (`lazyhome`, found
   by label as the stick does) and, with the update plan's state volume
   (U0, shared here as N5), so do `/apps`, `/conf` and `/logs`: settings and
   installed apps survive a reboot into a different build, which is the
   "real installation feel". The state volume is also where the boot OS
   reads the Wi-Fi profile and its sealed passphrase from, so the network
   you add once in Settings is the one the boot OS joins. Both need the
   GPT reader and the AHCI run on the box (the GRUB plan's K1/K2): until
   then the stick's `lazyhome` is `/home` and the boot OS carries a seeded
   profile (§8.5).
5. **The loop becomes: build, push, reboot the box, watch.** `push.py
   --machine box --reboot` marks the build as the box's next boot and asks
   `dbgd` to reboot it (one new control-tier method that asks `init`); the
   picker boots it with no wait; `dbgctl.py log --follow` shows it; the
   stick is never rewritten again. A broken build is one `push.py --id
   <previous>` away from the last good one, and the picker's "local copy"
   on the ESP boots with no network at all.
6. **iPXE is an option, not a step.** Revision 2 made iPXE over the cable
   the first stage. With Wi-Fi landed it buys only a loop a few weeks
   earlier, and only on a box with a cable; the spike tooling exists
   (`tools/netboot/spike/`, branch `claude/netboot-spike-prep`) and §13 has
   what one run showed. Keep it as `--ipxe` variants of the harness if the
   kexec work takes long; otherwise skip it.

## 2. The boot chain

```
 dev machine                     homelab (Wi-Fi)                    the box (Wi-Fi, LazyOS only)
 LAZYOS_NETBOOT=1 cargo build    tools/netboot/registry.py          UEFI firmware ─► ESP \EFI\BOOT\BOOTX64.EFI
   target/netboot/                 /srv/lazyos/<channel>/<id>/        └─ the loader reads kernel-x86_64 + ramdisk (the boot OS)
     kernel-x86_64                   kernel-x86_64 ramdisk                 └─ kernel ─► init ─► wifid, wlanmd (system profile from
     ramdisk                         manifest.json                              /conf: the state volume, or the seeded copy) ─► netd
     manifest.json                 GET /api/builds?mac=..  (JSON)           ─► netpicker: GET /api/builds, the menu, the countdown
 python tools/netboot/push.py ───► PUT /api/builds/.. (signed manifest)      ─► GET /files/<ch>/<id>/{kernel-x86_64,ramdisk} into RAM
                                   POST /api/machines/<name>/next (signed)    ─► manifest.sig against the embedded key, SHA-256 of both ─► init.Kexec
                                   GET /boot.ipxe        (iPXE, optional)   ─► the system kernel: ram0 = /, /home = LABEL=lazyhome,
                                                                                  / = LABEL=lazystate and /system = ram0p2 ro (N5)
                                                                          no network: the picker kexecs \EFI\lazyos\local\ after a countdown
```

## 3. What exists, what is missing

| Piece | State today | Needed |
|---|---|---|
| A kernel that boots on the box from an ESP | the stick's loader and kernel (boot-1); the ESP of the NVMe install plan's layout is FAT16, which the kernel reads | nothing |
| Wi-Fi | `wifid` (RTL8822CE, `libs/rtw88`), `wlanmd` (station, `libs/wlansta`), `netd` interface `wlan0` with metric 600; a **system** profile (`sys/net/wifi/networks/<id>` in `confd`, its passphrase sealed in `keyd`, stored only through `elevd`'s `net.wifi.system`) is auto-connected before any login; verified on the box: WPA2, 5 GHz, DHCP (#776) | the boot OS needs that profile and secret available with no desktop and, before K1/K2, no disk (§8.5); the cold-boot time to `connected` *to measure* (§13) |
| Wired fallback | RTL8168 `netdrv` on the box | nothing; `netd` prefers the cable (metric 100) when one is plugged in |
| Loader files and the RAM root | `build_support/usb_image.rs` writes `kernel-x86_64`, `boot.json`, `ramdisk` and the UEFI stage into the stick's FAT; `ram0` wins root selection; `FS:ROOT:ram0p2`; `/home` by `LABEL=lazyhome` | the same files as loose files in `target/netboot/` with `manifest.json` (N1) |
| Build identity | `LAZYOS_REVISION` in `uname` and About (#754) | `image.id=` in `lazyos.cfg`, logged as `BOOT:IMAGE:<id>`, shown in About (N1) |
| Starting a second kernel | none; `power` (syscall 21) knows off and reboot, only `init` calls it | **kexec** (B1, §8.2) |
| HTTP from a program | `fetch`/`curl` (`nettls`, plain HTTP too) over `netd` | the picker fetches in-process into a shared buffer, or spawns `fetch` into `/tmp` and maps the file; B2 decides |
| A window and a keyboard without a session | `xuid` and a single client (`LAZYOS_XUI_APP` images); `usbd`, `inputd` | the boot profile's manifest (§8.3) and `netpicker` |
| The box's disk | AHCI driver written, not run on the box (ahci-plan A4); GPT refused (`block/partition.rs`) | grub-install-plan K1/K2, shared, not duplicated here |
| State | `/apps`, `/conf`, `/logs` on the root (RAM on a stick or netboot); `pkgd` unpacks every core package at every boot | update-plan U0: a `state` volume as `/`, the OS volume read-only at `/system` (N5) |
| Writing the ESP from LazyOS | FAT is read-only in the kernel; nothing in user space writes FAT | the boot OS must be updatable from the running system (§8.4) |
| Remote reboot | `dbgd`'s control tier restarts and hot-reloads services | `power.reboot`, asking `init` (N4) |
| Registry, push, picker | nothing; `tools/net/*peers.py` are the model for a stdlib host server; `tools/netboot/spike/serve.py` already serves the payload with a per-request MB/s log | `tools/netboot/registry.py`, `push.py`, `install_box.py`, `run.py`, tests (N2, N3); `netpicker` (B2) |

## 4. Decisions

1. **The native boot OS is the path; iPXE is optional.** Alternatives for
   the boot stage: (a) the firmware's UEFI HTTP Boot (one file from a URL
   that DHCP or the setup screen provides; no menu, no Wi-Fi); (b) firmware
   PXE into the vendored loader's `Tftp` mode (a proxy-DHCP on the LAN, no
   choice, lockstep TFTP, no Wi-Fi); (c) iPXE (menu and HTTP, every wired
   NIC, no Wi-Fi); (d) LazyOS itself. Only (d) reaches the goal, and with
   Wi-Fi landed its missing piece is kexec alone. (c) stays available as
   `--ipxe` harness variants and an `install_box.py --ipxe` for a box with a
   cable, if the kexec work drags; it costs no OS code and the spike
   tooling exists, but it is not on the critical path and the box is not
   expected to be wired.
2. **The payload is the stick's payload.** One file table, one ramdisk
   writer (`usb_ramdisk::write`), one loader: `LAZYOS_NETBOOT=1` adds an
   output directory, not a second image format; a build with both switches
   writes identical bytes to the stick and to `target/netboot/`. The same
   profile rule holds (`usb_stick::check_profile`: `LAZYOS_USB=1` and a
   services or desktop session, because the box has no PS/2 port). The
   picker fetches `kernel-x86_64` and `ramdisk`; the loader's own files
   (`bootx64.efi`, `boot.json`) are in the directory for iPXE and the
   rescue copy and are otherwise unused.
3. **Builds are immutable and named; channels order them.** A build id is
   `<yyyymmdd-HHMMSS>-<rev8>[-dirty]`; a pushed id is never overwritten
   (the same id with other digests is refused). A channel (`dev`, `box`,
   `stable`) is an ordered list, newest first. Pruning keeps the last N per
   channel and every pinned build. The registry stores files and JSON and
   generates text; it never reads a byte of the ramdisk.
4. **What boots is signed; the wire is plain HTTP.** The dev machine holds
   an Ed25519 key pair (`tools/netboot/keygen.py`, once; the secret in
   `target/netboot/key.sec`, git-ignored, and never on the homelab). The
   build signs `manifest.json` (`manifest.sig`; the manifest carries the
   SHA-256 and size of every file, the update plan's U1 shape), the boot OS
   embeds the public key (`LAZYOS_BOOTOS_KEY=<pub>`), and the picker
   **refuses to kexec** a build whose manifest does not verify or whose files
   do not match it. The registry is then storage, not a trust anchor: a LAN
   attacker who replaces its files gets a refusal and a `PICKER:VERIFY:FAIL`,
   and reads can stay plain HTTP. Writes are authenticated by the same key,
   with no bearer token to capture: a build's `commit` is accepted when its
   `manifest.sig` verifies against a key in the registry's `keys.json` and
   every uploaded file matches the manifest; the other writes (`next`, pin,
   prune, delete) carry a signed body (action, nonce, timestamp) that the
   registry verifies and refuses to replay. The registry verifies with the
   `cryptography` package when present, else with the pure-Python RFC 8032
   verifier in `tools/netboot/ed25519.py` (tested against the RFC vectors);
   the picker verifies with `ed25519-dalek`, already in `nettls-crypto`. The
   local copy on the ESP is trusted as the ESP is (Secure Boot is off, the
   loader and kernel are unsigned), which the plan states rather than fixes.
5. **Home by label now, UUID later.** `home=LABEL=lazyhome`; the SSD's home
   partition is labelled `lazyhome`. `home=UUID=` comes with per-machine
   configuration (§12).
6. **The boot OS is a build profile of the one kernel, not a second OS.**
   `LAZYOS_BOOTOS=1` is a profile like `LAZYOS_DESKTOP=1` (§8.3): the same
   kernel, `init`, services and `xuid`, a shorter manifest and one app.
7. **The boot OS's network is the machine's system network.** `wlanmd`
   already auto-connects the best known system-scope profile before any
   login; the boot OS adds nothing to the station stack and asks for no
   passphrase. Where the profile and its sealed secret come from is §8.5:
   the state volume once the SSD is readable, a seeded copy in the boot
   ramdisk until then.
8. **Evidence first.** `BOOT:IMAGE:<id>` and `BOOT:VIA:kexec` on serial and
   in About; `PICKER:*` lines; the registry logs which machine fetched which
   build; `push.py` prints the id it pushed and `run.py` requires the kernel
   to report the same id. CI runs the whole chain under OVMF over virtio-net
   with a host-side registry; the radio is proven on the box.
9. **The box is LazyOS only.** The ESP is ours, FAT16, laid out so the
   firmware's default path boots the boot OS and `\EFI\lazyos\local\` holds
   a full system as the rescue copy (§7). The first install of the ESP and
   the partitions is done from a stick boot (the NVMe plan's installer with
   the GRUB plan's `lazyos-install` core) or, this once, from a live Linux
   stick with `install_box.py`; after that every update of the boot stage
   comes from the running LazyOS (§8.4).

## 5. The registry

**The registry ships as a Docker container**, because the homelab is managed
with Cosmos Cloud and a container is the unit it installs, updates and backs
up. `tools/netboot/Dockerfile` builds it from `python:3-alpine` with the
`tools/netboot/` scripts and nothing else (standard library only:
`http.server`, `json`, `hashlib`; the Ed25519 verifier of decision 4 is the
pure-Python one, so the image needs no compiler and no wheel). The container
runs as an unprivileged user, serves on one port, keeps everything under one
volume, and takes its settings from the environment:

| | |
|---|---|
| Image | `ghcr.io/va1erian/lazyos-registry:<tag>`, built and pushed by `.github/workflows/netboot.yml` on `main` and on tags, with `latest` following `main`; `docker build -t lazyos-registry tools/netboot` for a local one |
| Port | `8420/tcp` (`LAZYOS_REGISTRY_BIND`, default `0.0.0.0:8420`; Cosmos's reverse proxy may front it, but the box talks to the port directly, since the boot OS trusts signatures, not TLS) |
| Volume | `/srv/lazyos` (`LAZYOS_REGISTRY_ROOT`): builds, `channels.json`, `machines.json`, `access.log`, and `keys.json`; the one thing to back up |
| Environment | `LAZYOS_REGISTRY_KEYS` (path, default `/srv/lazyos/keys.json`; the public keys allowed to write), `LAZYOS_REGISTRY_KEEP` (default per-channel `keep`), `LAZYOS_REGISTRY_LOG` (`access.log` path, or `-` for stdout, Cosmos's log view) |
| Health | `GET /healthz` (200, the build count and free space under the volume), declared as the image's `HEALTHCHECK` |
| First run | an empty volume gets `channels.json`, `machines.json` and an empty `keys.json` written with a log line saying no key may push yet; `push.py --register-key` prints the line to paste into it (or the admin copies `key.pub` in) |

A `docker-compose.yml` beside the Dockerfile is the Cosmos Cloud import
(one service, the port, the volume, `restart: unless-stopped`), and the
same `registry.py` still runs bare (`python tools/netboot/registry.py --root
DIR --bind HOST:PORT`) for the harness and on a machine without Docker.
`tools/netboot/spike/serve.py` (the spike's single-build server with `Range`
and a per-request log) is its seed.

```
/srv/lazyos/
  builds/<channel>/<id>/{kernel-x86_64,ramdisk,bootx64.efi,boot.json,manifest.json}
  builds/<channel>/<id>.incoming/     (an upload in progress; renamed when complete and verified)
  channels.json     {"dev": {"keep": 10, "pinned": ["20261011-193200-2eeee936"]}, ...}
  machines.json     {"box": {"mac": "aa:bb:...", "channel": "dev", "timeout_ms": 5000,
                              "next": null, "last_boot": {"id": ..., "at": ...}}}
  access.log
```

| Method and path | Who | What |
|---|---|---|
| `GET /api/builds?mac=..` | `netpicker`, tools | JSON: every build's manifest and `manifest.sig` in channel order, this machine's channel, default and pending `next`. Read-only: nothing is consumed or recorded |
| `GET /files/<channel>/<id>/<name>` | `netpicker` (iPXE) | the file, with `Content-Length`; `Range` honoured |
| `POST /api/machines/<name>/booting {"id": ...}` | `netpicker`, after verify, before kexec | consumes `next` (one shot), records the attempt. A failed download or verify never reaches it, so `next` stays pending for the next try |
| `POST /api/machines/<name>/booted {"id": ...}` | the booted system (`netboot-ack`, a oneshot `init` row that reads `bootos.registry=` from its `lazyos.cfg` when the picker wrote one, §12), or `push.py --follow` relaying `BOOT:IMAGE` from `dbgd` | records `last_boot`. An attempt never confirmed shows as "attempted, not confirmed" |
| `PUT /api/builds/<channel>/<id>/<name>`, then `POST .../commit` | `push.py`, signed | files land in `<id>.incoming/`; `commit` verifies `manifest.sig` against `keys.json` and every file's digest against the manifest, then renames the directory into place, or deletes it |
| `POST /api/channels/<channel>/pin\|unpin\|prune` | `push.py`, signed body | pins, and pruning by `keep` |
| `POST /api/machines/<name>/next {"id": ...}` | `push.py`, signed body | the one-shot default for that machine's next boot |
| `DELETE /api/builds/<channel>/<id>` | signed body | refused for a pinned build |
| `GET /boot.ipxe?mac=..`, `GET /builds/<channel>/<id>.ipxe` | iPXE, optional | the menu and the four `imgfetch`/`chain` lines (the spike's `scripts.py` has the shape) |

A machine the registry does not know gets the `dev` channel and a 10 s
menu. `access.log` has one line per request with the client address and the
rate, so "did the box fetch the new ramdisk, and how fast?" is a `tail`.

## 6. The payload and the push

`LAZYOS_NETBOOT=1 cargo build` (the same inputs as `LAZYOS_USB_IMAGE=1`,
which it may be combined with) writes:

```
target/netboot/
  kernel-x86_64    the trimmed kernel
  ramdisk          usb_ramdisk::write: lazyos.cfg + the ext2 OS volume
  bootx64.efi      the vendored bootloader UEFI stage (for iPXE and the rescue copy)
  boot.json        the loader's config (the 1280x720 minimum mode, as the stick)
  manifest.json    {"id", "revision", "dirty", "built_at", "host",
                    "switches": ["LAZYOS_DESKTOP", "LAZYOS_NETD", "LAZYOS_WIFI", ...],
                    "files": {"ramdisk": {"size", "sha256"}, ...}}
```

The ramdisk's `lazyos.cfg` gains `image.id=<id>`; the kernel logs
`BOOT:IMAGE:<id>` after `BOOT:MEDIA` and About shows it under the revision.
`tools/boot/preflight.py` learns a `--netboot DIR` mode that applies its
existing checks to the directory (the spike's `extract_payload.py` already
does the reading half).

**Size.** The stick build measured on 2026-10-11 has a **308 MiB** ramdisk
(`usb-stick.md` quotes 104 MiB for an older profile). N1 records what grew
(the `LAZYOS_USB_ROOT_FREE` free space is downloaded too; the core packages'
unpacked room is counted in since #703; the docs) and the boot OS's
`lazyos.cfg` may set a smaller free space for netbooted systems whose state
lives on the SSD. At the box's measured Wi-Fi rate (§13) this number is the
boot time.

```bash
python tools/netboot/push.py                       # target/netboot -> $LAZYOS_REGISTRY, channel dev
python tools/netboot/push.py --channel box --machine box --reboot --follow
python tools/netboot/push.py --list | --pin ID | --prune
python tools/run_demo.py --netboot --push http://192.168.1.5:8420 --build-only
```

`push.py` reads the registry URL from `--registry`, `LAZYOS_REGISTRY` or
`tools/netboot/registry.local.json` (git-ignored) and the signing key from
`target/netboot/key.sec`, runs the preflight, checks `manifest.sig` itself
before uploading, uploads, commits, and prints the id. `--machine NAME` sets that machine's `next`; `--reboot` asks the box
through `dbgd` (N4) and `--follow` tails its log until `BOOT:IMAGE:<id>` and
`SHELL:DESKTOP:PASS` arrive, or a timeout. The launcher gets a *Netboot*
group on the Advanced tab (the switch, registry URL, channel, machine, a Push
button) wired through `tools/lazygui/catalog.py` with `test_catalog.py` rows.

## 7. On the box (LazyOS only, Wi-Fi)

The SSD's layout is the NVMe install plan's GPT with `ahci0` for `nvme0`
(ESP, slot A, slot B, `lazyboot`, `state`, `home`); this plan asks only that
the ESP be FAT16 and that `home` be labelled `lazyhome` and `state`
`lazystate`. Until K1/K2 land the kernel cannot read the SSD and the box
boots the stick as today; the ESP work can start regardless, because the
firmware reads the ESP, not the kernel.

The ESP:

```
\EFI\BOOT\BOOTX64.EFI        the vendored loader
kernel-x86_64, boot.json     the boot OS's kernel and loader config
ramdisk                      the boot OS's ramdisk (LAZYOS_BOOTOS=1; a few tens of MiB)
\EFI\lazyos\local\           kernel-x86_64 + ramdisk of a full system: the rescue copy the picker
                             kexecs when there is no network, or on request
\EFI\lazyos\ipxe.efi         optional (decision 1), a second firmware entry for a wired box
```

One firmware entry, the default path, so a cold box reaches the picker with
no key pressed; the picker reaches the registry over `wlan0` (or `eth0`
when a cable is in, `netd`'s metric decides) and, failing that within a
bounded wait, counts down to the local copy. The firmware keeps **Secure Boot
off** (the loader and kernel are unsigned) and **Fast Boot off** (USB input),
as for the stick.

`install_box.py` writes the ESP (the loader, the boot OS pair, the local
pair) and, with `--home /dev/sdaN` and `--state /dev/sdaN` once K1/K2 land,
formats those partitions with `tools/mkdisk`'s formatter or `lazyos-install`
(never `mke2fs` defaults: the kernel driver knows only the features those
write), after the device names are typed back. Until then the LazyOS stick
stays plugged in and its `lazyhome` is found late through `usbd`, as today;
remove it once the SSD home exists (decision 5's ambiguity).

**The debug loop**, from the dev machine:

```bash
python tools/run_demo.py --desktop --wifi --dbgd-control --netboot --build-only
python tools/netboot/push.py --machine box --reboot --follow      # PICKER:KEXEC ... BOOT:IMAGE:<id> ... SHELL:DESKTOP:PASS
python tools/dbg/dbgctl.py log --follow                           # the running box, as today
```

## 8. The boot OS

### 8.1 What it needs, what exists

| The stage needs | Exists | Missing |
|---|---|---|
| A kernel that boots on the box from the ESP | yes (the stick's loader and kernel; boot-1) | nothing |
| A Wi-Fi link with no one logged in | `wifid`, `wlanmd` auto-connecting a system profile, `netd` (#776, verified on the box) | the profile and its secret available to the boot OS (8.5); the cold-boot time *to measure* |
| HTTP | `netd` sockets; `fetch` | the picker's client (8.3) |
| A keyboard and a screen | `usbd`, `inputd`, `xuid`, xui | `netpicker` (`tools/xui/new_app.py`) |
| Somewhere to put a ramdisk | RAM; shared buffers and the per-process quota (`limits.rs`) | the boot OS's `lazyos.cfg` raises `shared_buffer_max` and `quota_user_memory` for its one client |
| Starting the downloaded kernel | **no** | **kexec** (8.2) |
| Updating the boot OS from LazyOS | no FAT writer | 8.4 |

### 8.2 kexec

- **The handover reuses the loader's own code.** The vendored UEFI stage is a
  thin shell over `bootloader-x86_64-common` (parse the kernel ELF, allocate
  and map its segments, build page tables, lay out `BootInfo`, switch). That
  crate is `no_std`, pinned in `vendor/bootloader`, and the kernel can depend
  on it. The kexec implements its `FrameAllocator` over the kernel's physical
  allocator and its mapper over `mem::layout`'s offset mapping, and calls the
  same `load_and_switch_to_kernel`. The `BootInfo` it builds carries the
  **firmware's memory map as the boot OS received it** (kept from its own
  `BootInfo` since boot), re-marked: the boot OS's own pages become usable
  (it is being discarded), the new kernel's frames, page tables, stack and
  the ramdisk are `Kernel` and `Bootloader` regions, the framebuffer and the
  RSDP pass through unchanged, `ramdisk_addr/len` point at the downloaded
  image in place. One copy of the ramdisk in memory, not two.
- **The downloaded files arrive in a shared buffer.** The picker fetches into
  a `Buffer` (the shared-page object every interface may carry) and hands it
  to a new `init` method, `Kexec(kernel: Buffer, ramdisk: Buffer, reason:
  String) -> (accepted: Bool, phase: String)`, declared in `idl/init.midl`
  beside `Shutdown`, whose `mode`, `reason` and `force` contract is left as
  it is (a `Buffer` is an object field, so it cannot ride in `Shutdown`'s
  `reason`). The kernel's kexec op pins the frames and reads the kernel ELF
  from them; the ramdisk's frames are what the new kernel is told about.
  The op refuses anything that is not a LazyOS kernel (it checks the ELF's
  `bootloader` config section before touching a frame) and any ELF whose
  segments fall outside usable RAM or overlap.
- **Devices are quiet first, every one of them.** `init.Kexec` runs the
  ordinary stop order (apps, then services in reverse dependency order, the
  lifecycle for durable state), so `wifid` has powered the chip off
  (`WIFID:STOP`, the controlled exit the driver already has), `netdrv`
  stopped its rings, a block driver flushed and stood its disk by. The one
  deliberate exception of a power-off, `usbd` outliving the stop so the
  kernel's `sync_all` can flush a `/home` on a stick through it
  (`shutdown.md`, `OUTLIVE`), is handled by `power(kexec)` itself: after
  `sync_all` it ends every task that still holds a device claim, which is
  what task teardown already does for a killed driver (bus mastering off,
  DMA blocks freed; a `UserDisk` fails fast once its provider is gone), and
  then **refuses the handover if any claim is still live**, logging the
  owner. Only then does it disable interrupts, mask the I/O APIC and the
  LAPIC timer, and jump. The new kernel initialises every device from
  scratch, as after a firmware boot; the xHCI BIOS handoff `usbd` performs is
  idempotent, and `wifid`'s bring-up already copes with a chip the previous
  owner left running (`rtw88::pci::quiesce`).
- **What the new kernel sees** is a normal `bootloader` boot (`BOOT:MEDIA:
  uefi`, from the memory map's firmware types, preserved) plus a
  `BOOT:VIA:kexec` line from one optional `BootInfo` field added to the
  vendored common crate, defaulting to "loader": the only loader patch, kept
  with the interrupt-disable patch already in `vendor/bootloader`.
- **Not supported, by design:** kexec from a kernel that has woken
  application processors (S8 does not exist yet; when it does, the APs are
  parked with INIT before the jump), kexec of a BIOS-booted kernel (the boot
  OS is UEFI only), and anything but a LazyOS kernel.
- **Tests** (AGENTS.md's two kinds): `kexec_suite` under `cfg(lazyos_tests)`
  builds `BootInfo`s from synthetic maps (UEFI shapes with holes, regions
  above 4 GiB, a ramdisk straddling a hole, a framebuffer inside a region)
  and checks every region is accounted for exactly once, the new kernel's
  frames never overlap the ramdisk or the firmware's reserved regions, and a
  hostile ELF (segments outside RAM, overlapping, no config section, a 4 GiB
  `memsz`) is refused before any frame is taken; a soak plans and discards
  10,000 handovers over random maps and leaks no frame. End to end (B4): the
  boot OS kexecs the dev image under OVMF and the dev image reaches the
  desktop with `BOOT:VIA:kexec`, then `powerctl kexec` from that desktop
  chains a third time, so a leak or a device left running shows up.

### 8.3 The boot profile and `netpicker`

`LAZYOS_BOOTOS=1` (decision 6) is a session profile: `init`'s manifest keeps
`messengerd`, `logd`, `confd`, `keyd`, `devd`, `wifid`, `wlanmd`, `netdrv`,
`netd`, `usbd`, `inputd`, `xuid`, and starts **`netpicker`** as the only
client, as `LAZYOS_XUI_APP` images do today. No `pkgd`, packages, docs,
accounts, sound or `elevd`. The ramdisk is the kernel, a dozen static
binaries, the Wi-Fi firmware blob (200 KiB) and the font: a few tens of MiB
*to measure* (B2), against the desktop's 308 MiB, so the firmware loads it
from the ESP quickly and the box's floor is the kernel-to-`connected` time
(§13). Its `lazyos.cfg` carries `bootos.registry=`, `bootos.machine=`,
`bootos.local=` (the rescue pair's path) and the raised memory limits.

`netpicker` is one window: the channels and builds from `GET /api/builds`
(id, date, revision, switches, size), the default preselected, a countdown
honouring the registry's `timeout_ms` and `next`, a "Local copy" row and a
"Shell" row (the Terminal, for a box with no serial port), the link state
(`system/net/+/wifi/state`, `netd`'s address) and a progress bar with the
bytes per second. Keyboard first: arrows, Enter, Escape stops the countdown;
the mouse works because `xuid` does. It fetches into a shared buffer,
verifies `manifest.sig` against the embedded public key and then the
manifest's SHA-256 of both files (a forged or corrupt download is a message
and the menu again, never a boot; decision 4), posts `booting`, and calls
`init.Kexec`. Evidence: `PICKER:UP`, `PICKER:LINK:<if>:<addr>`,
`PICKER:LIST:<n>`, `PICKER:FETCH:<id>:<bytes>:<ms>`, `PICKER:VERIFY:PASS|FAIL`,
`PICKER:KEXEC:<id>|local`. With no link within a bounded wait (association
plus DHCP, tens of seconds at most) or no registry, it says so and counts
down to the local copy, so a cold box always boots something.

### 8.4 Updating the boot OS from the running system

The boot OS changes rarely (a kexec fix, a driver, a picker feature) but must
be updatable without a stick, since the box has no other OS and the kernel
does not write FAT. Two routes, chosen in B3:

1. **A FAT16 writer in user space** (`libs/fat16w`, host-tested and fuzzed;
   the spike's `fat16_format.py` is a pure-Python formatter whose output the
   kernel's reader accepts, which fixes the on-disk shape), used by a
   privileged `bootosd` holding write access to the ESP partition and
   nothing else (the update plan's narrow-capability shape). The kernel and
   the ramdisk must switch **as one generation**: two renames leave a window
   in which the loader reads one file from each build. So the writer lays
   both new cluster chains down first (`fsync`), then switches both
   root-directory entries in **one sector write**: `kernel-x86_64` and
   `ramdisk` are kept adjacent in the same root-directory sector (the
   formatter and the writer place them so and refuse a root where they are
   not), a 512-byte sector write is atomic on the medium, and the old chains
   are freed only afterwards. A tear before that write leaves the old pair
   whole, after it the new pair whole. B3's torn-update test kills QEMU at
   every step of that sequence.
2. **The boot OS lives in an ext2 slot** and the ESP holds only the update
   plan's slot-aware loader (U2's forked UEFI stage, which reads a kernel
   out of an ext2 volume through `libs/ext2fs`). LazyOS writes ext2 today;
   the ESP is never written after install. More convergence, but it waits
   for U2.

Either way the boot OS is itself a build the registry carries (channel
`bootos`) and `push.py --bootos --machine box` installs it through `dbgd`
and `init`. Until B3 lands, a stick refreshes it, which is also the recovery
when a boot OS does not come up.

### 8.5 The Wi-Fi profile and its secret

The desktop joins a network through `wlanmd`: a **system** profile is written
by `elevd` (`net.wifi.system`, after an administrator approved the prompt)
under `sys/net/wifi/networks/<id>` in `confd`, and its passphrase is sealed
in `keyd`'s store (`/conf/svc/keyd/secrets`, under a machine key kept
beside it); `wlanmd` auto-connects the best known system network before any
login and fetches the PMK from `keyd` (`WifiPmk`, answered to uid 912 alone).
The boot OS runs the same three services; what differs is where `/conf` is:

- **Target (with N5 and K1/K2):** the boot OS mounts the state volume
  (`lazystate`) as `/` like the full system, so `/conf` is the one the
  desktop writes. The network added once in Settings is the one the boot OS
  joins; a changed passphrase is picked up at the next boot; nothing is
  copied anywhere. The boot OS mounts it read-only except for what `confd`
  and `keyd` need (`confd` tolerates a read-only store, logged; `keyd`
  caches the PMK sealed beside the secret, which it can skip).
- **Interim (RAM root, SSD unreadable):** the boot ramdisk's `/conf` is
  seeded at install with the system profile and `keyd`'s sealed store *and*
  its machine key copied from the full system's `/conf` (the copy is as
  secret as the ESP, which is not at all: Secure Boot is off and the disk is
  unencrypted, the same statement the full system already makes). `install_box.py`
  does the copy from the stick-booted system's `/conf`, so the passphrase is
  typed exactly once, in Settings, through the administrator prompt. A
  build-time `LAZYOS_BOOTOS_WIFI=<ssid>:<passphrase>` is **not** offered: a
  passphrase in a build environment ends up in logs.

Either way the picker never sees a passphrase or a PMK, and `wlanmd`'s refusal
of mixed-mode (TKIP) and PMF-required networks applies to the boot OS as to
the desktop.

### 8.6 Cost against iPXE

| | iPXE (optional) | Boot OS |
|---|---|---|
| Code in the OS | none | kexec (kernel + tests), the profile, `netpicker`, later the ESP writer |
| Network | cable only | Wi-Fi or cable, `netd` decides |
| Time to the menu | 2 to 3 s *to measure* | kernel to picker, bounded below by `wifid` bring-up, association and DHCP: *to measure* (§13; `usbd`'s 15 s per dead USB device from boot-1 is the known risk) |
| Download | iPXE's HTTP: 45 MB/s through QEMU's slirp (one run, §13) | `netd` TCP over `wlan0`: *to measure* on the box |
| Extra boot | none | the system kernel starts a second time (immediate, no firmware) |
| UI | text menu | a LazyOS window |
| Fallback | second firmware entry | the picker's local copy |

## 9. Phases

Each phase ends shippable. Python stays under 500 lines per file (several
small modules in `tools/netboot/`, as `tools/boot/` does); kernel work ships
correctness and soak tests under `kernel/src/tests/` and passes
`python tools/test/run.py --accel none`. Every phase that boots adds its
variant to `tools/netboot/run.py`.

| Phase | Deliverable | Evidence |
|---|---|---|
| **Q0** The floor, measured (one evening, no code in the OS) | On the box, with the current Wi-Fi stick (`run_demo.py --desktop --wifi --dbgd-control --usb-image`) and `dbgd`: the kernel-to-`connected` time from the log's timestamps (`WIFID:FW:READY`, `WLANMD:STATE wlan0 connected`, `NETD:ADDR`), and the download rate over `wlan0` by fetching the 308 MiB ramdisk from `tools/netboot/spike/serve.py` on the dev machine (`fetch http://<dev>:8420/files/ramdisk -o /tmp/r`, the server's MB/s log and `/tmp` being RAM). Optional: `qemu_spike.py` once more with the server's `--timeout 600000`, to see whether the loader's ramdisk read through iPXE's volume finishes (§13) | §13 filled: association time, MB/s over Wi-Fi, the stick's kernel-to-`init` time; the iPXE verdict if run |
| **N1** Payload | `build_support/netboot.rs`: `LAZYOS_NETBOOT=1` writes `target/netboot/` (decision 2), `manifest.json` and, with `LAZYOS_NETBOOT_KEY=<key.sec>`, `manifest.sig` (Ed25519 over the manifest's bytes, `ed25519-dalek` from `nettls-crypto` on the host; a build without the key is pushable but no boot OS will run it); `image.id=` in `lazyos.cfg`, `BOOT:IMAGE:<id>`, About shows it; `preflight.py --netboot`; `run_demo.py --netboot`; the launcher group and `catalog.py`; the 308 MiB accounted for and the netboot free space decided | `cargo test -p build-support-tests` (digests match, the id format, a dirty tree marked, both switches write identical bytes); `mount_suite`: `image.id` parsed and logged, a bad value refused without affecting the mount; `tools/lazygui/test_catalog.py` |
| **N2** Registry and push | `tools/netboot/registry.py` (+ `store.py`, `api.py`, `menu.py` for the optional iPXE text), `push.py`, `keygen.py`, `ed25519.py` (the RFC 8032 verifier), the `Dockerfile` and `docker-compose.yml` (§5: unprivileged user, one port, one volume, environment settings, `/healthz`), `README.md` (including the Cosmos Cloud import); the spike's `serve.py` and `scripts.py` folded in | `test_registry.py` against a loopback server: atomic commit (a torn upload leaves no build), a bad or missing `manifest.sig` refused, a key not in `keys.json` refused, digest mismatch refused, the same id with other bytes refused, pin and prune, a signed body on every write and a replayed nonce refused, `..` and absolute names refused, `Range`, the JSON (and the menu) for a known machine (channel order, default, `next` pending until `booting`, consumed exactly once, `booted` recorded only by the acknowledgement) and for an unknown MAC; `test_ed25519.py` on the RFC vectors; `test_push.py` against a fake registry; `test_container.sh` (skipped without Docker): build the image, run it on a temp volume with a throwaway key, push a payload from the host, read it back, `/healthz` green, the process is not root |
| **B1** kexec | `kernel/src/power/kexec.rs` (+ `kexec/` for the map and the ELF checks), the `power(kexec)` kind (the sync, the claim teardown, the refusal while a claim is live), `init.Kexec(kernel, ramdisk, reason)` in `idl/init.midl`, `powerctl kexec KERNEL RAMDISK` for a developer at the Terminal, the `BOOT:VIA` field in the vendored common crate | `kexec_suite` (8.2, plus: a fake claim held across the sync is torn down and a claim that cannot be ended makes the op refuse); `tools/test/run.py --accel none` green; `tools/netboot/run.py --kexec`: the dev image boots under OVMF over virtio, `powerctl kexec` of a test copy in `/system/share/netboot/`, the second kernel logs `BOOT:VIA:kexec` and reaches the desktop, then a third |
| **B2** The boot OS | `LAZYOS_BOOTOS=1` (8.3): manifest, `netpicker` (`tools/xui/new_app.py`, permissions from a `LAZYOS_LABEL_TRACE=1` run), the HTTP client over `netd`, the signature check (`LAZYOS_BOOTOS_KEY=<key.pub>` embedded, `ed25519-dalek`), `bootos.*` cfg lines and limits; `run_demo.py --bootos`; the launcher; `target/bootos/{kernel-x86_64,ramdisk}` pushed to channel `bootos` by `push.py --bootos` | `cd xui-app && cargo test -p netpicker` (list parsing with unknown fields, a hostile manifest, the countdown, a bad signature and a digest mismatch each refused before any kexec); `run.py --bootos`: OVMF boots an ESP image carrying the boot OS, virtio-net to the host registry (`wifisim` over the second card where `tools/wifi/sim_run.py` already provides it, so the Wi-Fi path runs in CI too), the picker lists, fetches the pushed dev build, `PICKER:VERIFY:PASS`, `PICKER:KEXEC`, the dev build's `BOOT:VIA:kexec` and desktop; a corrupted file gives `PICKER:VERIFY:FAIL` and the menu; the registry down gives the local copy; the ramdisk size recorded in §13 |
| **N3** Harness and CI | `tools/netboot/run.py` as above with `--home-disk` (a virtio disk from `tools/mkdisk --home-volume` mounts at `/home`), `--esp`, `--kexec`, `--bootos`, optional `--ipxe` (the spike's path: slirp TFTP, OVMF PXE, the menu); OVMF found as `tools/boot/run.py` finds it plus QEMU's own `share/edk2-x86_64-code.fd` on Windows; `test_run.py` | `.github/workflows/netboot.yml` (Linux runner, OVMF, artifacts like `screenshots.yml`; also builds the registry image, runs `test_container.sh` against it, and pushes it to `ghcr.io/va1erian/lazyos-registry` on `main` and tags) |
| **B3** Install and update | `install_box.py` (§7: the ESP, the local pair, the seeded `/conf` of 8.5; later `--home`, `--state`); the update route of 8.4; `push.py --bootos --machine box` | `cargo test -p fat16w` (host, fuzz seeds, `fsck.fat -n` clean in CI) if route 1; harness: update the boot OS from the running system, reboot, the new boot OS's `PICKER:UP` with its id; a torn update (QEMU killed before, during and after the one-sector switch) always boots a kernel and ramdisk of the **same** build, checked by `BOOT:IMAGE` against the kernel's revision |
| **B4** The box | the boot OS on the box's ESP over Wi-Fi; `docs/netboot.md` (the homelab service, the box, firmware settings, "the picker does not appear", recovery is the local copy or the stick); the `compat/hardware.md` row | kernel-to-picker, association and download times over the radio in §13; a pushed build boots with no cable and no key pressed; a file saved to `/home` survives; the registry stopped, the box boots the local copy on its own |
| **N4** The loop | `dbgd` control method `power.reboot` (asks `init.Shutdown(reboot)`, never the kernel's `power`; refused without the control tier; `dbgwire` allowlist and tests); `push.py --machine --reboot --follow`; `machines.json` documented | `cargo test -p dbgwire`; `tools/dbg/run.py` gains the reboot; on the box: build, push, reboot, desktop, under a minute, hands off |
| **N5** State on the box (update-plan U0, shared) | `state=LABEL=lazystate`: the state volume is `/` (`/apps`, `/conf`, `/logs`), the ramdisk's OS volume read-only at `/system` (`/docs/os` to `/system/docs`, one `libs/fhs` constant), `pkgd` reconciles core packages up and down; the boot OS mounts the same volume (8.5 target); `run_demo.py --state-disk` | `mount_suite`: the three-volume layout, `EROFS` under `/system`, a missing state volume falls back to the RAM root with a log line; harness `--state-disk`: a setting and an installed app survive a second network boot of a *different* build, a downgrade restores the older core packages, the boot OS joins the network from the state volume's profile; `tools/accounts/run.py` still blocked everywhere |

**Order:** Q0 now: it costs nothing and its numbers (association time,
MB/s over the radio, the 308 MiB) decide how much the ramdisk must shrink
and whether iPXE is worth keeping. N1 and N2 together, independent of the
box. **B1 is the critical path** and depends on nothing but N1's payload;
start it in parallel. B2 needs B1 and N2; N3 wraps them for CI; B3 and B4
follow; N4 is small and makes B4 pleasant. N5 is the update plan's U0 and
the largest item; it needs K1/K2 and can land anywhere after N3. Nothing
waits for the Wi-Fi track any more.

## 10. Verification

Modelled on `tools/boot/run.py` and `tools/net/run.py`: the verdict is
markers, the registry's own record, and pixels.

- `python tools/netboot/run.py [--no-build] [--home-disk] [--state-disk]
  [--esp] [--kexec] [--bootos] [--ipxe] [--memory 2G] [--accel auto]`
  writes `shots/netboot/{serial.log,registry.log,screen.png,report.json}`
  (seconds from QEMU start to the picker, to the kernel, to the desktop;
  bytes served and the rate; the ramdisk sizes).
- `test_registry.py`, `test_push.py`, `test_run.py`: every judge and rule
  fails when it should. `preflight.py --netboot` before every push.
- Kernel: `kexec_suite` in `python tools/test/run.py --accel none`.
- Real hardware: `docs/compat/hardware.md`'s row for the box gains "network
  boot: <s> to picker, <s> associated, <MB/s> over Wi-Fi, <s> to desktop";
  photos of the picker and About in `compat/kabylake/boot-N/`.

## 11. Risks and open questions

1. **kexec correctness** is the risk: a device left doing DMA, a page table
   the new kernel does not expect, a memory map that calls a region free
   while the ramdisk sits in it. The design leans on the shutdown order that
   already quiesces drivers and on the loader's own crate for the handover;
   the harness chains three kernels so a leak or a stray device shows.
   Expect the first bare-metal kexec to find one thing QEMU does not model
   (boot-1 found three for USB); `dbgd` and the on-screen panic are the
   evidence.
2. **The Wi-Fi cold-boot floor.** `wifid` loads firmware and calibrates
   (hundreds of milliseconds), `wlanmd` scans unless the BSS was heard in the
   last 30 s (a cold boot has heard nothing: 22 channels), then joins and
   DHCP runs. If Q0 measures tens of seconds, the picker's countdown runs in
   parallel with association (it shows the link state and starts the fetch
   when the link is up), and `wlanmd` could remember the last BSS's channel
   in `confd` to join without a scan (a station-stack item, not netboot's).
3. **Download time over the radio.** 308 MiB at 10 MB/s is 30 s, at 30 MB/s
   10 s; the LazyOS TCP stack's rate over `wlan0` is unmeasured (Q0). Levers:
   a smaller free space for netbooted systems (N1), a compressed ramdisk the
   picker inflates into the shared buffer (`zstd` has a `no_std` decoder; no
   loader involvement in the kexec path), a channel of builds without docs,
   and, with N5, no core-package room in the ramdisk at all since `/apps`
   lives on the SSD.
4. **The boot OS's own boot time.** `usbd` spends up to 15 s on a dead USB
   device (boot-1), and the box's keyboard and mouse are full speed. If the
   picker takes long to appear, the remedy is in `usbd` (shorter bounded
   retries for the boot profile), not here; Q0 measures the stick's
   kernel-to-`init` time as the floor.
5. **Memory while loading.** One copy of the ramdisk in a shared buffer
   counted against the picker's quota; the boot OS's `lazyos.cfg` raises
   `shared_buffer_max` and `quota_user_memory` for it. Fine with 8 GiB; the
   harness runs with `--memory 2G` to keep it so.
6. **The seeded `/conf` (8.5 interim)** copies `keyd`'s machine key onto
   the ESP. It is no weaker than the full system's own `/conf` on an
   unencrypted disk, but it is a second copy to forget: `install_box.py`
   says so, and N5 removes it. A changed passphrase before N5 means re-running
   the install step.
7. **Updating the boot OS** (8.4) is a chicken-and-egg until B3: a boot OS
   that fails to come up is fixed by booting a stick, which the box must
   therefore keep able to do (Fast Boot off). B3's torn-update test makes
   this rare.
8. **Where is `/home` while the SSD is not reachable?** On the stick, which
   means `usbd` must come up before the late home mount (it does,
   `INIT:HOME mounted`), with `usb-storage.md`'s timeouts. The SSD home
   (K1/K2) ends this; it is tracked in the GRUB install plan.
9. **The signing key is the trust anchor** (decision 4): the dev machine's
   `key.sec` can sign a kernel the box will run, so it is as sensitive as
   `dbgd`'s control key and lives only there. Rotation is a boot OS built
   with the new public key (B3's update route) and a `keys.json` edit on the
   homelab; losing the key means a stick. What stays unauthenticated is the
   ESP itself (the loader, the boot OS and the local copy), because Secure
   Boot is off; `docs/netboot.md` says so on its first page.
10. **Reboot by `dbgd`** adds a remote way to stop the machine to a tool
    whose first version was read-only by design, and the control tier's key
    already buys the whole box (`dbgd.md`). It stays in that tier, goes
    through `init` like every other stop, and `dbgwire`'s allowlist test
    names it.
11. **If iPXE is kept** (decision 1): the one run so far (§13) shows the
    loader reading `kernel-x86_64` and `boot.json` from iPXE's virtual
    volume and then stalling on the 308 MiB `ramdisk` read; whether that is
    a single-call size limit in iPXE's file protocol or slowness is unknown,
    and the `sanboot` fallback is untested. Settle it only if the variant is
    wanted.

## 12. Later

- **Several signing keys and a signed boot OS.** `keys.json` and the boot OS
  can carry more than one public key (a CI release key beside the dev key),
  and the ESP's own files can be verified by a loader patch once the update
  plan's shim exists; until then the ESP is trusted as the disk is.
- **Per-machine configuration**: the picker holds the ramdisk in RAM and its
  1 MiB FAT part is at a fixed offset, so `machines.json` lines
  (`home=UUID=`, `display.max`, `limit.*`, `diag.hold`) can be written into
  `lazyos.cfg` before the kexec, and one build boots many machines.
- **The boot OS as the A/B shim.** With the update plan's control block the
  picker shows the two installed slots beside the registry's builds and
  kexecs a slot's kernel from its ext2 volume; U2's forked UEFI stage then
  shrinks to "load the boot OS".
- **A compressed ramdisk** and a channel of builds without docs (risk 3).
- **Netbooting QEMU itself**: `run_demo.py --from REGISTRY` boots the last
  pushed build without a local build.
- **Other machines:** the boot OS on a USB stick is a netboot stick for any
  PC LazyOS boots, wired or wireless, with the same registry.

## 13. Measurements

Filled as they are taken. From the spike tooling's one run on the dev
machine (QEMU q35, OVMF, WHPX, `tools/netboot/spike/`, 2026-10-11):

| What | Value |
|---|---|
| Stick build's payload | kernel 2.3 MiB, `bootx64.efi` 170 KiB, **ramdisk 308 MiB** (`usb-stick.md` quoted 104 MiB for an older profile) |
| iPXE | official `x86_64-efi/ipxe.efi`, iPXE 2.0.0+ (`ga57ea`), SHA-256 `893dc378...bf26a4`; OVMF PXE-booted it over slirp TFTP; an `autoexec.ipxe` in the TFTP directory was honoured |
| iPXE download of the ramdisk through slirp | 7.1 s, 45 MB/s |
| Loader through iPXE's virtual volume | `kernel-x86_64` and `boot.json` read ("Reading configuration from disk was successful"); the `ramdisk` read did not finish in several minutes (one run, killed; unexplained) |

To take in Q0 and later: the box's kernel-to-`init` time, `WIFID:FW:READY`
to `WLANMD:STATE connected` to `NETD:ADDR`, MB/s over `wlan0` for the 308 MiB
ramdisk, the boot OS ramdisk size (B2), kernel-to-picker on the box (B4).
