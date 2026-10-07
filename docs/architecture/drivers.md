# Drivers: `devd` and the second NIC and sound drivers (D7)

**What it is.** The stage that closed [driver-plan](../driver-plan.md) D5 and
D7 (issue #497): the device manager `devd` that starts drivers for the devices
it finds, a second NIC driver (the Intel 8254x, QEMU's `e1000`) and a second
sound driver (Intel High Definition Audio) that needed **no new `dev_*` op**,
the in-kernel virtio-blk on the modern virtio transport, and a seeded fuzz of
the device syscall. The kernel device core underneath is
[devices.md](devices.md); the NIC and sound stacks above are
[networking.md](networking.md) and [audio.md](audio.md).

**Key files**

| Path | Role |
|---|---|
| `user/src/bin/devd.rs`, `devd/devices.rs` | The device manager: inventory, match, `StartDriver`, `os.lazy.devd.v1`, `system/devices/<id>` |
| `libs/devmatch` | The static driver manifest (`MANIFEST`) and its matching, host tested |
| `idl/devd.midl`, `idl/init.midl` (`StartDriver`) | The interfaces |
| `user/src/bin/init/drivers.rs` | `StartDriver`: the driver rows `devd` may ask for |
| `libs/nicdrv/src/rings.rs` | `NicRings`, the trait a NIC implements under the shared engine |
| `libs/e1000` | The 8254x: registers, reset, station address, legacy descriptor rings |
| `user/src/bin/netdrv/{device,card,virtio_card,e1000_card,rings}.rs` | `netdrv`'s card front and its two back ends |
| `libs/hda` | HDA: controller, CORB/RIRB, codec walk, path programming, stream, cursor |
| `user/src/bin/sndd/{card,device,virtio_card,hda_card}.rs` | `sndd`'s card front and its two back ends |
| `kernel/src/block/virtio/{regs,modern,io}.rs` | virtio-blk's legacy and modern transports |
| `kernel/src/tests/dev_suite/fuzz*.rs` | The `dev_*` argument fuzz and the lifecycle soak |

## `devd`

```
 kernel inventory ──▶ devd (_devd, no caps) ──StartDriver(row, id)──▶ init
                       │  libs/devmatch             (program, uid, args, restart)
                       │                                       │ spawn  dev=<id>
                       └─ system/devices/<id> (retained)       ▼
                          os.lazy.devd.v1 Devices()       netdrv / sndd ──claim──▶ kernel
```

`devd` runs as `_devd` (uid 906) with **no** capabilities. At start it reads
the kernel's read-only inventory (`dev_*` op 10, the same one `devctl` uses),
matches each function against `devmatch::MANIFEST` (exact vendor/device ids, or
a PCI class for an interface that is the same on every vendor's part: HDA), and
for each driver row picks the first matching device in enumeration order. It
asks `init` to start that row with `os.lazy.init.v1.StartDriver(driver,
device)`; any other device the same row matches is `busy` (a row's Messenger
name is unique, so one card per driver).

`init` keeps the authority: the driver rows (`SNDD_ROW`, `NETDRV_ROW` in
`state.rs`) carry the program, the credentials (`_snd`, `_net`), the arguments
and the restart policy, and only the running task of the `devd` row may call
`StartDriver`. `devd` can name a row and a device id, nothing else; the driver
receives the id as `dev=<id>` and claims exactly that device, which the kernel's
class rules still judge. A row `init` does not have (the image does not ship
that driver) answers `ENOENT`, which `devd` reports as `nodriver`: QEMU gives a
sound-only image its default e1000, for instance.

Then `devd` registers `os.lazy.devd`, tells `init` it is ready, and re-reads
the inventory twice a second: a device a driver claims becomes `claimed`, one
its driver lets go becomes `released`. Every change is logged
(`DEVD:DEVICE`) and published on the retained `system/devices/<id>` topic. A
crashed driver is restarted by `init` (its row's policy), not by `devd`.
`devctl drivers` prints `Devices()`.

The broker used to let only uid 0 publish under `system/` (`logd` treats that
namespace as authentic), which also silently dropped `netdrv`'s
`system/net/<nic>/link`. A dedicated system uid may now publish its own
subtree and nothing else (`messengerd/filter.rs`: `_devd` `system/devices/`,
`_net` `system/net/`).

**Switch.** With a sound or network driver in the image, `init` starts `devd`
instead of the static rows; `LAZYOS_DEVD=0` (`run_demo.py --no-devd`, the
launcher's Drivers group) keeps the old behaviour, where `init` starts
`netdrv`/`sndd` at boot and each takes the first card it knows. A kernel booting
a driver directly (no `init`) is unchanged.

## The second NIC: Intel 8254x

`nicdrv::Engine` (client rings, frame policy, receive filter, statistics) is
now generic over `NicRings`: poll completed receive frames, send one frame,
reap transmits. virtio-net's `Queues` and `e1000::Rings` implement it.
`libs/e1000` is the 8254x's legacy descriptor rings over one DMA block: every
descriptor owns a 2048-byte slot; a completion's length, status and errors are
untrusted (a frame spread over descriptors, a length past the slot or an error
bit is dropped and counted); a received frame is copied out before it is
examined; the receive tail moves the "gap" along the ring, the transmit tail is
the doorbell. Bring-up is `CTRL.RST`, the station address from `RAL0/RAH0` or
the EEPROM, link from `STATUS.LU`, interrupt causes from `ICR` (read to clear,
which deasserts the line before `irq_ack`).

`netdrv` drives either card: `device.rs` finds and claims (or takes `devd`'s
`dev=<id>`), `virtio_card.rs` and `e1000_card.rs` bring the card up, and
`card.rs` is the front the service and self-test use. Settings come from
`sys/dev/net/virtio-net/*` or `sys/dev/net/e1000/*`. The ops it uses are the
ones virtio-net used: claim, `cfg_write` (decode, bus master, INTx),
`map_bar`, `dma_alloc`, `irq_enable`/`irq_ack`.

## The second sound card: Intel HDA

`libs/hda` is the generic HDA driver logic: link reset and codec discovery
(`STATESTS`), the CORB/RIRB verb rings (with `RIRBCTL.RINTCTL` and
`RIRBSTS` cleared per response: QEMU's controller stops walking the CORB once
`RINTCNT` responses are pending), a codec walk (audio function group, widget
capabilities, connection lists with ranges, pin configuration defaults) that
finds the best attached output pin (line out, speaker, headphone) and the
shortest path through mixers and selectors to a converter, path programming
(power, selects, pin control, EAPD, every amp unmuted at its 0 dB step), format
words, the output stream descriptor with one BDL entry per period, and a cursor
that turns the cyclic link position into completed periods. Everything a codec
or controller reports is bounded before use.

`sndd`'s stream and session code speak in periods submitted and completed (the
virtio model). The HDA card presents that over a cyclic buffer: the staging
is the buffer, DMA starts once every period holds samples (or as soon as the
driver waits for one), a period completes when the link position has passed it
and is then zeroed (so a late refill replays silence, not old samples), and a
stop lets one more period play out (the codec buffers about a period ahead of
the link position). Periods must be 128-byte multiples (BDL alignment);
`audiod`'s 4096 and `beep`'s 8192 are.

## virtio-blk on the modern transport

The kernel's virtio-blk drives a function through the virtio 1.x transport
whenever it has the virtio PCI capabilities (a transitional `1af4:1001` or a
modern-only `1af4:1042`), with the same `libs/virtio::Transport` the
userspace drivers use (`Transport::setup_queue_at` takes the kernel's own queue
addresses). The BAR windows are mapped once with `mem::mmio::map_kernel`,
after their bases are checked against the device table, only as far as the
structures reach. A legacy-only function (QEMU's `disable-modern=on`, which the
harnesses' boot disk uses) keeps the 0.9.5 I/O window. Both share the ring, its
static memory and layout; `virtio/regs.rs` is the only place they differ.
Booting from a transitional disk (`run_demo.py`'s default) mounts the root over
the modern transport.

## The `dev_*` fuzz

`dev_fuzz_syscall_arguments` calls every op, unknown ones included, from three
drivers (one without `CAP_DEV_CLAIM`) on four synthetic devices with distinct
hazards (I/O BARs over the PIC and the PCI config ports, unassigned and
unaligned memory BARs, a bridge, a platform device), with arguments half valid
and half edges and noise, and pointers checked by user-pointer validation.
After each call: a value or a known errno; a handle that is not the caller's
own live claim refused with exactly `EBADF`, its own never with `EBADF`; no
hazard reachable; owner, generation and claim quota equal to the model. Four
seeds of 6000 calls (`LAZYOS_DEV_FUZZ_SEED=<n>` replays one); a floor on what
was reached keeps it from passing vacuously. `dev_fuzz_lifecycle_soak` drives
valid sequences (claim with and without an interrupt endpoint, arm, map, port
I/O, DMA, interrupts taken and acknowledged, release, crash) for 30 000 steps,
twice with the same seed: the second run must give back every frame.

The soak found a frame leak: every address space that mapped a shared buffer
kept its shared-window PDPT after exit, and a spawned child inherited its
parent's window with every buffer in it. The window is now each address
space's own ([physical-memory.md](physical-memory.md), teardown invariants).

## Verification

```bash
cargo test -p e1000 -p hda -p devmatch -p nicdrv -p virtio
LAZYOS_TEST_FILTER=dev_fuzz python tools/test/run.py --accel none
LAZYOS_TEST_FILTER=virtio python tools/test/run.py --accel none   # incl. virtio_modern_*
python tools/net/run.py --nic e1000            # the capture judges the 8254x
python tools/net/run.py --services             # devd starts netdrv (DEVD:* markers)
python tools/sound/run.py --card hda           # the recording judges HDA
python tools/sound/run.py --card hda --services --mix
python tools/screenshot/qemu_session.py --image target/lazyos.img --out shots/devd \
    --script tools/screenshot/examples/devd_desktop.json \
    --extra-arg=-audiodev --extra-arg=none,id=a0 \
    --extra-arg=-device --extra-arg=intel-hda,id=hda0 \
    --extra-arg=-device --extra-arg=hda-output,bus=hda0.0,audiodev=a0   # desktop, LAZYOS_XUI_AUTOSTART=term
```

## Not done

An IOMMU (a driver with `DMA` is still trusted like the kernel): issue #615.
MSI/MSI-X and the I/O APIC landed with #616 ([interrupts.md](interrupts.md)):
`netdrv` and `sndd` take MSI-X on virtio, `sndd` MSI on HDA. `devd` does not apply the `confd` device policy of
[driver-config-plan.md](../driver-config-plan.md) (enable/disable, binding
overrides) and does not watch for hot-plug; one device per driver row; the
`e1000e`/`igc` families, HDA capture, HDMI codecs and a DSP-mode (SOF)
controller.
