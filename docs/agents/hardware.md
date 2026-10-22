# Display, drivers and hardware

Moved out of [`AGENTS.md`](../../AGENTS.md) to keep it short. HiDPI, the Intel GPU service, devd, interrupt routing, USB, sound, FAT sticks and installing beside Linux.

## HiDPI (a 720p desktop at 2x)

`python tools/run_demo.py --desktop --hidpi` (GUI: *HiDPI* on the Simple tab,
*Display mode* on the Advanced tab) builds with `LAZYOS_DISPLAY_MODE=2560x1440`:
the kernel switches QEMU's std VGA to that mode after boot (`display.mode` in
`lazyos.cfg`, `kernel/src/display/bochs.rs`; the BIOS bootloader stops at
1280x720), and the desktop draws a 1280x720 layout natively at 2x. `xuid`
picks the scale (`sys/ui/scale`: `auto`, `1`, `2`; auto is 2 from 2560x1440)
and hands it to clients with `GetOutput`; xui apps run at `96 * scale` DPI.
The wire stays in physical pixels. Code laid out in pixel constants uses
`xui_app::hidpi` (`design_bounds`, `rect`, `design_rect`). Plan and status:
[`docs/hidpi-plan.md`](../hidpi-plan.md).

**Hi-res** is the same 2560x1440 screen at scale 1, a roomy desktop whose text
and widgets keep their normal pixel size: `python tools/run_demo.py --desktop
--hires` (GUI: *Hi-res* on the Simple tab, excluding *HiDPI*; *Display scale*
on the Advanced tab; `--hires` = `--display-mode 2560x1440 --display-scale 1`).
`LAZYOS_DISPLAY_SCALE` writes `display.scale=` to `lazyos.cfg`, which sets the
boot console's scale and is `xuid`'s default for `sys/ui/scale` when that key is
unset (`UiScale::from_cfg`); a set key wins.

The mouse pointer is an antialiased vector sprite (`libs/cursorgfx`) drawn at
`sys/ui/cursor/size` x the scale, with shapes (arrow, busy, I-beam, hand, four
resize arrows) that the compositor picks over window borders and apps set with
`SetCursor` (`os.lazy.display.cursor.v1`, a scope a package lists in its
manifest); see [`docs/cursor-plan.md`](../cursor-plan.md). Sessions:
`cursor_shapes.json`, `cursor_docs.json`, `cursor_xdemo.json`.

```bash
LAZYOS_DESKTOP=1 LAZYOS_XUI_AUTOSTART=term LAZYOS_DISPLAY_MODE=2560x1440 LAZYOS_RESET_OS=1 cargo build
python tools/screenshot/qemu_session.py --image target/lazyos.img     --out shots/hidpi --script tools/screenshot/examples/hidpi_apps.json
python tools/screenshot/pngstats.py shots/hidpi/*.png --expect-width 2560 --expect-height 1440
python tools/run_demo.py --desktop --hires   # 2560x1440 at 1x; session: hires_apps.json (serial: XUID:SCALE:1 setting=1 screen=2560x1440)
LAZYOS_TEST_FILTER=display_mode python tools/test/run.py --accel none
```

## Intel GPU service (`gpud`, `LAZYOS_INTELGFX=1`)

`gpud` (`user/src/bin/gpud/`, uid `_gpu` 915 holding only `CAP_DEV_CLAIM`,
rules in `libs/gpupolicy`) is stage G1 of
[`docs/kabylake-gpu-plan.md`](../kabylake-gpu-plan.md), **read-only**:
`devd` starts it for the Kaby Lake HD Graphics 620 (`8086:5916`, Gen9) or the
Alder Lake-P UHD Graphics (`8086:46b3`, display 13), matched only in an image
built with the switch (`devmatch::INTELGFX_MANIFEST`); it claims
the function without an interrupt, maps BAR 0 behind a read-only
`intelgfx::Mmio` (reads only inside `libs/pciprobe`'s range allowlist), reads
and decodes what the firmware left (`libs/intelgfx::adopt`, one
`adopt::Display` per generation, `gen9` and `d13`: pipe, DDI, link mode,
timing, pixel clock, CDCLK, plane, the GGTT pages of the scanout) and
prints it as `GPU:*` lines, measures the refresh rate from the pipe's frame
counter (`libs/intelgfx::vblank`: `GPU:VBLANK`, `GPU:UNDERRUN`), then serves
`os.lazy.gpu.v1` (`idl/gpu.midl`, `GetState` from that snapshot, `GetBeam`
live: frame counter and scan line). It writes no display register, takes no
interrupt and sets no mode. A claim switches the function's decode and bus
mastering off (`dev::syscall::quiesce`), so `gpud` switches them back on at
once and never exits on purpose while it holds the claim (a release would
stop the kernel console's writes into the aperture).

```bash
python tools/run_demo.py --intelgfx                  # QEMU has no such GPU: gpud is not started there
python tools/run_demo.py --desktop --intelgfx --dbgd --usb-image --build-only   # a stick for either box
python tools/dbg/dbgctl.py --host <ip> log --source programs | grep GPU:       # what the firmware left
cargo test -p intelgfx -p gpupolicy -p devmatch      # decoders against both boxes' real register dumps
LAZYOS_TEST_FILTER=gpu python tools/test/run.py --accel none   # _gpu's class rules through claim
```

A claimed device is refused by `dbgd`'s `mmio.read` unless the image has
`LAZYOS_DBGD_MMIO_CLAIMED=1` (`--dbgd-mmio-claimed`); `pci-config` always
works. `gpud` run by hand with no such device (the Terminal as root, or a
console image) prints `GPU:NO-DEVICE:FAIL` and exits 0.

## Device manager (`devd`) and driver choices

With a sound or network driver in the image, `init` starts `devd` (uid 906, no
capabilities): it matches the enumerated devices against `libs/devmatch` and
asks `init` (`StartDriver`) to start `netdrv`/`sndd` for the card it found,
publishing `system/devices/<id>`; `LAZYOS_DEVD=0` (`run_demo.py --no-devd`)
keeps the static rows. `netdrv` drives virtio-net or an Intel 8254x
(`run_demo.py --net --nic e1000`), `sndd` virtio-sound or Intel HDA
(`run_demo.py --sound --sound-card hda`); the launcher's Advanced tab has a
Drivers group for all three, and `devctl drivers` shows `devd`'s view. See
[`docs/architecture/drivers.md`](../architecture/drivers.md).

**Several network cards (WP1, `docs/wifi-prerequisites-plan.md`).** Every
network card gets its own `netdrv`, named `eth0`, `eth1`, ... in enumeration
order (`devd` picks the name, `StartDriver` carries it as `ifname=`), and
serves `os.lazy.net.nic/<ifname>`; there is no bare `os.lazy.net.nic`. `netd`
finds the cards by listing the registry, gives each an interface (own DHCP,
own `sys/net/<if>/*` configuration), sends new traffic by the lowest-metric
default route (wired 100, wireless 600) and takes `resolv.conf` from that
interface. A card that vanishes is a detached interface, not an error. `netctl
if` lists them; the Network app has a *Next card* button. A new driver for a
NIC registers `NicInfo.kind` (`wired` or `wireless`).

## Interrupt routing (I/O APIC, MSI)

The legacy lines go through the I/O APIC when the MADT names one, and a
userspace driver whose device has MSI or MSI-X gets a vector of its own
(`irq_enable` returns 0 INTx, 1 MSI, 2 MSI-X; a virtio driver on MSI-X calls
`Transport::use_msix(0)`). `LAZYOS_IRQCHIP=pic` and `LAZYOS_MSI=0`
(`run_demo.py --irqchip pic --no-msi`, the launcher's Drivers group) restore
the 8259 and INTx. The net, sound and USB harnesses take `--irq-path msi|pic`
and judge it (`tools/irqpath.py`, `DEV:MSI:PASS`, `HW:IRQCHIP:`); see
[`docs/architecture/interrupts.md`](../architecture/interrupts.md).

```bash
python tools/test_irqpath.py
python tools/net/run.py --irq-path pic
python tools/sound/run.py --card hda           # MSI
python tools/usb/run.py                        # qemu-xhci on MSI-X
```

## USB audio and several outputs (`LAZYOS_USB_AUDIO=1`)

`usbd` binds USB speakers, headsets and DACs (UAC 1.0 and 2.0 playback,
`usbd/audio*.rs` over `libs/usbaudio`: descriptors, alternate and rate choice,
isochronous TDs with Start Isoch ASAP, explicit feedback) and serves
`os.lazy.usb.audio.v1` (`idl/usb_audio.midl`) under `os.lazy.usb.audio` while
one is bound; `sndd usb=1` (init row `sndd-usb`, `_snd`) drives it as the card
`usb-audio0`. Every card registers as `os.lazy.audio.card/<id>`, and `audiod`
plays through one *output*: the one selected (`SelectOutput`, the Volume tray
menu's Output rows, `mixer output <id>`; kept per user in
`user/<uid>/audio/output`), else the best kind present (USB, HDMI, built-in,
virtual: a headset plugged in takes over, unplugging falls back). Opt-in until it has run on the Kaby
Lake and Alder Lake boxes: [`docs/usb-audio-plan.md`](../usb-audio-plan.md).

```bash
cargo test -p usbaudio -p xhci -p audiomix             # descriptors, pacing, isoch TRBs, output choice
python tools/sound/usb_run.py                          # QEMU usb-audio: 440 Hz + 880 Hz recorded and judged
python tools/sound/usb_run.py --hotplug                # + a virtio card: two outputs, unplug and replug
python tools/run_demo.py --desktop --usb-audio --dbgd --usb-image --build-only   # the boxes' stick
```

Markers: `USBD:AUDIO:BIND|UP|START|STOP|RATE|FAIL|XFER`, `SNDD:USB:CARD|WAIT|GONE`,
`AUDIOD:OUTPUTS`, `AUDIOD:CARD id=<id>`, `VOLUME:OUTPUTS`, `VOLUME:OUTPUT:<id>`.
Session `tools/screenshot/examples/volume_outputs.json` (the tray menu with two
outputs).

## Sound harness

The virtio-sound driver (`sndd`) and the system mixer (`audiod`,
[`docs/audio-plan.md`](../audio-plan.md)) are verified by listening: QEMU
records what the guest plays (`-audiodev wav`) and a detector measures the
recording. One command builds with `LAZYOS_SOUND=1`, boots headless, records and
checks it:

```bash
python tools/sound/run.py                          # driver tone + beep through the mixer
python tools/sound/run.py --mix                    # two clients as one chord, a half-volume tone
python tools/sound/run.py --starve --services     # a stream run dry: exactly one underrun event (#453)
python tools/sound/run.py --services               # devd starts sndd (_snd); init supervises audiod (_audio)
python tools/sound/run.py --card hda               # an Intel HDA controller and codec (issue #497)
python tools/sound/run.py --machine q35 --virtio-disk
python tools/sound/test_analyze_wav.py             # the detectors' own tests
python tools/sound/test_mixcheck.py
cargo test -p virtio -p virtio-snd -p pcm          # the driver libraries
cargo test -p audiomix -p audioclient              # the mixer engine and the client library
```

Applications play sound through `libs/audioclient` (`PlaybackStream`), never by
opening the card. Do not claim an audio change works from the serial markers
alone; the verdict is the recording. See `tools/sound/README.md` and
`docs/architecture/audio.md` (including why a driver must never free a DMA
buffer while its device runs).

## USB harness

The USB HID driver (`usbd`, [`docs/usb-hid-plan.md`](../usb-hid-plan.md)) is
verified by what reached `inputd` (architecture: `docs/architecture/usb.md`): QEMU runs with `qemu-xhci`, a USB keyboard
and mouse and no i8042, and the judge checks every key edge `inputd` saw came
from `usbd` and that the descriptors are QEMU's. See `tools/usb/README.md`.

```bash
python tools/usb/run.py                  # build (LAZYOS_SERVICES=1 LAZYOS_USB=1), boot, judge
python tools/usb/run.py --ps2            # PS/2 and USB side by side
python tools/usb/run.py --hotplug 200    # unplug/replug over QMP: nothing stuck, DMA bounded
python tools/usb/run.py --tablet         # usb-tablet: report descriptor, absolute cursor
python tools/usb/run.py --restart        # usbd crashes holding a key: released, restarted, re-enumerated
python tools/usb/run.py --hub            # keyboard and mouse behind a usb-hub, then the hub unplugged
python tools/usb/run.py --full-speed     # USB 1.1 devices on root ports
python tools/usb/run.py --controllers 2  # two xHCI controllers, keyboard on the second
python tools/usb/test_judge.py           # the judge fails when it should
cargo test -p usbhid -p xhci             # descriptor/report parsers and xHCI rings (host)
```

Under TCG the harness paces input (USB is polled; see the README): KVM runs are
the verdict.

USB sticks (`/home` on the boot stick, `docs/architecture/usb-storage.md`)
have their own harness, `tools/storage/README.md`:

```bash
python tools/storage/run.py              # two boots: write /home/user on the stick, power off, read it back; e2fsck
python tools/storage/test_judge.py       # the judge fails when it should
cargo test -p usbmsc --features fuzz     # Bulk-Only Transport and SCSI (host, fuzz seeds)
```

## FAT sticks at `/media` (`fatfuse`, `libs/fatvol`)

A FAT12/16/32 stick plugged in while someone is logged in is mounted by
`mountd` at `/media/<label>` through a `fatfuse` daemon (a FUSE provider that
holds a raw claim on the disk, `docs/architecture/usb-storage.md` "Removable
volumes"; plan and what differs from it: `docs/usb-hotplug-plan.md` "As
built"). The FAT code is `libs/fatvol` (`rust-fatfs` pinned, plus boot-sector
validation, a write-back sector cache, MBR/GPT discovery), host-tested; the
`fatstick` example makes and reads sticks without `mkfs.fat`. A kernel disk
slot (`usb<n>`) is reused after an unplug: an operation takes the slot's
**generation** when it starts and fails once the slot holds another disk, so
never keep a `&'static` reference to a provider disk for longer than one
operation (the late `/home` mount is the one holder, and pins its slot).

```bash
cargo test -p fatvol -p mounttable                                  # the library and the automount rules
cargo test -p fatvol --release soak -- --ignored                    # a long model soak
cargo run -p fatvol --example fatstick -- make s.img --label X --file a.txt=hi
LAZYOS_BUSYBOX=<busybox> python tools/storage/fat_run.py            # build, plug, use, pull, replug, power off, judge
LAZYOS_TEST_FILTER=provider python tools/test/run.py --accel none   # slot reuse, generations, claims
```

`fat_run.py` plugs the stick in over QMP (`blockdev-add` + `device_add`: QEMU
drops a `-drive` when its device is pulled), writes, pulls it, plugs it in
again and reads the host copy with the same library and `fsck.fat -n`
(Docker). Daemons are ended with `SIGKILL` by `mountd`: a `SIGTERM` to a
native daemon parked in its provider `NEXT` was seen not to land before the
power-off sync.

## Installing beside Linux (`lazyos-install`, `LAZYOS_INSTALLER=1`)

`tools/install` (`lazyos-install`, a static musl Linux program) installs the
stick onto two GPT partitions of a disk Linux owns and adds a LazyOS entry to
that Linux's GRUB ([`docs/installing-beside-linux.md`](../installing-beside-linux.md),
plan [`docs/grub-install-plan.md`](../grub-install-plan.md)). It includes the
`build_support/os_*.rs`/`usb_*.rs` modules by path (one set of manifest
rules); GPT is `libs/gpt`, which the kernel reads disks with too
(`block/gpt.rs`); the UEFI loader reads its files from its own directory
(`vendor/bootloader/uefi/src/boot_dir.rs`). `--image FILE` (test mode) runs
any command against a disk image, no root.

```bash
python tools/run_demo.py --desktop --installer --build-only   # the stick, with install/lazyos-install
cargo test -p gpt -p lazyos-install
python tools/install/run.py               # disk image, install, boot it alone on AHCI/OVMF, power off, host check, boot again
python tools/install/test_judge.py        # the judge fails when it should
LAZYOS_TEST_FILTER=partition python tools/test/run.py --accel none   # MBR + GPT kernel cases
```
