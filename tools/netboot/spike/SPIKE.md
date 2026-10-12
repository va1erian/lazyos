# Q0 spike: iPXE hands LazyOS its loose files (checklist)

Phase Q0 of [`docs/netboot-plan.md`](../../../docs/netboot-plan.md) (section 9;
decision 2 in section 4; menu and `autoexec` shapes in sections 5 and 7).
Everything here was prepared on 2026-10-11/12 under QEMU on the Windows dev
machine; the verdict lines at the end are for you to fill in. The tools are
stdlib Python, every file under 500 lines, with tests that need no network and
no QEMU (`python -m unittest discover -s tools/netboot/spike`).

**Trust.** iPXE boots unauthenticated code over plain HTTP, by design: the
registry's files are fetched and run with no signature check
(`docs/netboot-plan.md`, decision 1 and risk 11). This path is optional and for
measurement only; the signed path is the LazyOS boot OS, whose picker verifies
`manifest.sig` before it kexecs anything. So `--box-host` (and everything the
tools here write for a real machine) is for a lab network you control, never a
shared one. `qemu_spike.py` exposes an unauthenticated QMP monitor on a
loopback port; it is for a single-user dev machine.

## Questions

1. Does the vendored UEFI loader (`vendor/bootloader/uefi/src/main.rs`,
   `load_file_from_disk`) read `kernel-x86_64`, `boot.json` and `ramdisk` from
   the virtual volume iPXE puts on the device handle of the image it chains,
   after `imgfetch --name <name> <url>` of each?
2. Is an `autoexec.ipxe` beside `ipxe.efi` honoured when iPXE is started from a
   filesystem (the box's ESP)?
3. Fallback: does `sanboot http://.../esp.img` of an ESP-only disk image work
   under OVMF?
4. How long does the ramdisk take to download under QEMU (and on the box)?

## Things that differ from what the plan assumed

* The official binary is `https://boot.ipxe.org/x86_64-efi/ipxe.efi`;
  `https://boot.ipxe.org/ipxe.efi` is a 404. `fetch_ipxe.py` uses the former.
* The ramdisk of `target/lazyos-usb.img` is **308 MiB** (322,961,408 bytes), not
  ~104 MiB: the stick image is a desktop build. Download times scale with it.
* **The menu auto-selects its default after 5 s** (that is the plan's shape:
  `choose --default ... --timeout 5000`), so the whole fetch-and-chain starts
  by itself. To sit at the menu, run `serve.py --timeout 600000`.
* Under PXE, `autoexec.ipxe` in the TFTP directory *is* run (see "Observed"),
  so the spike boots into the menu without typing. That says nothing about
  question 2 (a file beside `ipxe.efi` on a FAT filesystem).

## Before you start (once)

From the worktree root (`E:\repos\lazyos\...`). `target/` is git-ignored.

```bash
python tools/netboot/spike/extract_payload.py --image target/lazyos-usb.img --out target/netboot/payload
python -I tools/netboot/spike/fetch_ipxe.py --out target/netboot/ipxe --box-host <dev machine LAN address>
python tools/netboot/spike/make_esp_image.py --payload target/netboot/payload     # only for the sanboot fallback (question 3)
```

`QEMU` is found at `C:\Program Files\qemu\qemu-system-x86_64.exe`; its own
`share\edk2-x86_64-code.fd` is the OVMF code and `edk2-i386-vars.fd` the
variable store (copied fresh to `target/netboot/OVMF_VARS.fd` on every run).

## The run

1. **Terminal 1, the server:**
   `python tools/netboot/spike/serve.py --root target/netboot/payload --bind 127.0.0.1:8420 --timeout 600000`
   (the guest reaches it as `http://10.0.2.2:8420`; the log line per request
   has the client, path, status, bytes, seconds and MB/s).
2. **Terminal 2, QEMU with a window:**
   `python tools/netboot/spike/qemu_spike.py --serial target/netboot/serial.log`
   (`--accel none` forces TCG; `--headless --shot-at 20,60,120` for screenshots;
   `--mode rom` uses the NIC's own iPXE ROM instead of the official binary;
   `--machine pc` if q35 misbehaves; the exact command line is printed first).
   OVMF has no disk, so it PXE-boots `ipxe.efi` from slirp's TFTP.
3. **Watch the window**: OVMF's `>>Start PXE over IPv4`, `NBP file downloaded`,
   then `iPXE 2.0.0+ (ga57ea)`, `autoexec.ipxe... ok`, `http://10.0.2.2:8420/boot.ipxe... ok`, the menu,
   then progress lines for `ramdisk` (the longest step).
4. **Watch the serial log** (`target/netboot/serial.log`; OVMF and iPXE console
   output land there too). Success looks like, in order:
   `INFO : UEFI bootloader started`, `Reading configuration from disk was
   successful`, `Trying to load ramdisk via Disk`, `Loaded ramdisk`, then the
   kernel's `BOOT:MEDIA:uefi`, `FS:ROOT:ram0p2`, `PKGD:PROVISION:DONE ...
   failed=0` and `SHELL:DESKTOP:PASS`. `Trying to load ramdisk via Tftp` or
   `Failed to load kernel` means the virtual volume was not read (then see the
   fallback below and note which file the loader asked for).
5. **If the loader hangs after `Trying to load ramdisk via Disk`** (see
   "Observed"): leave it for a few minutes first and note whether the CPU is
   busy, then try `--accel none` and a smaller ramdisk to separate "slow" from
   "stuck". Do not start patching the loader; record it.

## If the menu fails: by hand in the iPXE shell

Press Esc/`shell` at the menu, or Ctrl-B early. Then:

```
dhcp
imgfetch --name kernel-x86_64 http://10.0.2.2:8420/files/kernel-x86_64
imgfetch --name boot.json     http://10.0.2.2:8420/files/boot.json
imgfetch --name ramdisk       http://10.0.2.2:8420/files/ramdisk
imgstat
chain --name bootx64.efi      http://10.0.2.2:8420/files/bootx64.efi
```

`imgstat` lists the fetched images with their sizes (the names are what the
loader will open). `chain http://10.0.2.2:8420/build.ipxe` runs the same four
lines from the server. If the DHCP/TFTP step cannot reach the server, check that
`serve.py` is up and that the port is free (`netstat -ano | findstr 8420`).

## Fallback: `sanboot` of an ESP image (question 3)

Only if question 1 fails. `make_esp_image.py` wrote `target/netboot/payload/esp.img`
(MBR + one FAT16 partition, type 0x0E, `EFI/BOOT/BOOTX64.EFI`, `kernel-x86_64`,
`boot.json`, `ramdisk`; read back with preflight's FAT reader, **never booted by
firmware yet**). At the iPXE prompt: `chain http://10.0.2.2:8420/sanboot.ipxe`,
or by hand `sanboot --no-describe http://10.0.2.2:8420/files/esp.img`. iPXE keeps the
whole image in RAM, so use `--memory 2G` or more (the plan notes the doubled
footprint). A failure to start the loader from the SAN disk is the answer; do
not debug it.

## Question 2 and the box

`autoexec.box.ipxe` (written by `fetch_ipxe.py --box-host`) is the plan's section 7
file (`exit` on failure). For the box: copy `ipxe.efi` and `autoexec.ipxe`
(that file renamed) to the current stick's FAT or an ESP, start it from the
firmware's boot menu, and see whether the menu appears with no typing. Run
`serve.py --bind <LAN address>:8420 --guest-host <LAN address>` on the dev
machine. Under QEMU this cannot be tested through PXE (the file came from TFTP
there); a filesystem-started test needs `ipxe.efi` and `autoexec.ipxe` on a FAT
disk image, which is a second QEMU configuration not built here
(`boot.ipxe.org/relnotes.md` documents `ipxe.usb` with an editable
`autoexec.ipxe`, which suggests it works; unverified).

## Numbers to record (section 13)

* the loader verdict (virtual volume / sanboot / needs a patch);
* the `autoexec` verdict; iPXE version and SHA-256 (`target/netboot/ipxe/ipxe.sha256`);
* seconds from QEMU start to the menu (serial/screen timestamps; `--shot-at`);
* the ramdisk download: the `GET /files/ramdisk` line of the server log (seconds,
  MB/s), under QEMU, then on the box over the cable;
* seconds from the loader's start to `SHELL:DESKTOP:PASS`; on the box the stick's
  kernel-to-`init` time (the boot OS's floor);
* the ramdisk size (308 MiB for this build).

## Observed while preparing (not the spike's verdicts)

Commands run from the worktree root; outputs abridged.

`extract_payload.py` on the real stick (0.4 s):

```
$ python tools/netboot/spike/extract_payload.py --image E:/repos/lazyos/target/lazyos-usb.img --out target/netboot/payload
kernel-x86_64       2432560 bytes  c58f0832d74562b5...
boot.json               209 bytes  d900edf4dd8998cb...
ramdisk           322961408 bytes  e4f7fbb70f392a83...
bootx64.efi          174080 bytes  7b938b5bd5fc7856...
ok: bootx64.efi: BOOTX64.EFI is a PE32+ x86-64 EFI application (machine=0x8664 magic=0x20b subsystem=10)
```

The sizes equal `python tools/boot/preflight.py` (kernel 2432560, ramdisk 322961408,
BOOTX64.EFI 174080) and `sha256sum` of the extracted ramdisk matches the manifest.

`fetch_ipxe.py` (network): 1,164,288 bytes, SHA-256
`893dc3781534dd07b367fed4b81a6be3bb5f1f170d3e56d55a2b1ff1cbbf26a4`, downloaded
2026-10-11 20:59:50Z, iPXE `2.0.0+ (ga57ea)` (boot.ipxe.org `version.txt`).

`serve.py` on loopback (`curl`): `/boot.ipxe`, `/build.ipxe`, `/sanboot.ipxe` answered with
the guest-host URLs; `Range: bytes=100-199` gave `206`, `Content-Range: bytes 100-199/2432560`;
`bytes=999999999-` gave `416`; the full ramdisk gave `200`, 322,961,408 bytes, matching SHA-256.
Log excerpt: `GET /files/ramdisk 200 322961408 B 0.61 s 530.8 MB/s` (loopback).

Tests: `python -I -m unittest discover -s tools/netboot/spike` ran 67 tests, OK;
`python tools/check_file_length.py` OK. `make_esp_image.py` on the real payload: 311.9 MiB,
FAT16 with 39,778 clusters of 8 KiB, read back file by file.

**Incidental QEMU run (went further than intended).** I launched
`qemu_spike.py --headless --timeout 150 --shot-at 25,60,110` once, meaning to stop at
the menu. Because the menu auto-selects after 5 s, the guest ran on by itself before I
looked, and I stopped QEMU (and the server) as soon as I saw it. What the serial log
and server log showed, unjudged:

* OVMF PXE-booted `ipxe.efi` from slirp TFTP (`NBP filesize is 1164288 Bytes`) with
  `-machine q35 -accel whpx` and the NIC ROM off (`mode pxe` works).
* iPXE printed `autoexec.ipxe... ok` (from the TFTP directory), configured net0, fetched
  `http://10.0.2.2:8420/boot.ipxe`, drew the menu, counted down from 5, then fetched
  `build.ipxe`, `kernel-x86_64`, `boot.json`, `ramdisk` (7.13 s, 45.3 MB/s on the
  server log) and `bootx64.efi`.
* The loader started and printed `UEFI bootloader started`, `Reading configuration
  from disk was successful`, `Trying to load ramdisk via Disk`, and the serial log
  ends there (last write 02:26:46, the moment the chain began), with no `Loaded
  ramdisk` when I killed QEMU a few minutes later. So the kernel and `boot.json`
  appear to have come through iPXE's virtual volume (a `Disk`-mode read; no TFTP
  fallback line), and the 308 MiB ramdisk read either takes minutes or hangs. I did not
  investigate which. Screenshot taken at 25 s: `target/netboot/shots/shot_0025s.png`
  (iPXE configuring net0); the later shots were not taken.

Treat these as leads, not verdicts: nothing was repeated, timed carefully or screenshot
past the loader's first lines.

## Verdicts (fill in)

1. Loader reads the three files from iPXE's virtual volume:
2. `autoexec.ipxe` beside `ipxe.efi` on a filesystem:
3. `sanboot` of `esp.img` under OVMF:
4. Ramdisk download time under QEMU / on the box:
