# Bluetooth harness

[docs/bluetooth-plan.md](../../docs/bluetooth-plan.md) phases B2, B3 and B4: `usbd`
binds a USB Bluetooth controller and serves it to `btd`, which runs the BR/EDR
and LE HID host ([docs/architecture/usb.md](../../docs/architecture/usb.md),
"Bluetooth controllers"). QEMU has had no Bluetooth emulation since `-bt` was
removed, so the controller is **`fakebt`**, a host program that serves a fake
class `E0/01/01` dongle over **usbredir** (`-chardev socket ... -device
usb-redir`). `fakebt` is Rust, not Python, because the controller it serves is
`libs/btsim`'s `SimController` (a deterministic controller model with a
keyboard and a mouse peer, behind the plan's `Controller` trait). It needs
QEMU built with usbredir (`qemu-system-x86_64 -device help | grep usb-redir`;
the Windows build in `C:\Program Files\qemu` has it).

```bash
python tools/bt/run.py                # B2: build, boot, judge the bring-up
python tools/bt/run.py --no-build     # reuse target/lazyos.img
python tools/bt/run.py --unplug       # pull the dongle out after the bring-up
python tools/bt/run.py --hotplug      # the dongle appears 20 s after the connection, not at boot
python tools/bt/run.py --pair         # B3: pair the mouse and the keyboard; text and motion arrive
python tools/bt/run.py --reboot       # B3: pair, power off, boot the same image: it types at boot
python tools/bt/run.py --refuse       # B3: an unbonded peer pages the host and types: refused
python tools/bt/run.py --kill         # B3: btd dies holding a key: nothing stuck, it reattaches
python tools/bt/run.py --le-pair      # B4: an LE keyboard (passkey) and mouse pair; text and motion arrive
python tools/bt/run.py --le-reboot    # B4: pair over LE, power off, boot the same image: it types at boot
python tools/bt/run.py --le-rpa       # B4: as --le-reboot, the devices change address on every connection
python tools/bt/run.py --le-refuse    # B4: a Just Works LE keyboard is refused; a stranger is never connected
python tools/bt/run.py --fw intel     # B7: an Intel radio in its bootloader; btd downloads firmware, then --pair (also intel-tlv, intel-recovery, intel-reboot, realtek)
python tools/bt/test_judge.py         # the judges fail when they should
cargo test -p fakebt -p btusb -p btpolicy -p bthost -p btsdp -p bthid -p btgatt -p btsim -p btsmp
python tools/run_demo.py --bt         # an interactive boot with the fake dongle plugged in
```

The image `run.py` builds is the harness build: `LAZYOS_BT_TRACE=1` makes `init`
start `btd` with `trace=1` (a `BT:KEY` line per key edge and the pairing
passkey, which a normal build never prints) and `pair=` the two peers'
addresses (`LAZYOS_BT_PAIR`), a bounded pairing window until B5's interface
exists. `--kill` builds with `LAZYOS_BT_CRASH_TEST=1` as well. Every HID mode
boots a copy of the image (`shots/bt/disk.img`), so one run's bonds never leak
into another; `--reboot` boots the same copy twice (`shots/bt/boot1`,
`shots/bt/boot2`) and `fakebt` keeps the peers' link keys in
`shots/bt/peers.state` between the two.

The verdict is the guest's serial log. `judge.py` (the transport): `USBD:BT:BIND`
for interface 0 only, with the dongle's descriptors byte for byte; `btd` running
as `_bt` with only `CAP_INPUT_SOURCE`; `BT:CTRL:PASS` carrying the version,
manufacturer and last two address octets the simulated controller answered with
(the full address must never appear); and, with `--unplug`, `USBD:BT:UNBIND`,
the `USBD:DETACH` of a `bt` function, `USBD:BT:WITHDRAW` and `BT:CTRL:GONE`.
`judge_hid.py` (the HID modes), by what reached `inputd`: every `INPUTD:KEY` is
a `BT:KEY` edge of `btd`, in order, and the text typed is `INPUTD:TEXT`.

| Mode | Peers | The verdict |
|---|---|---|
| `--pair` | keyboard (Passkey Entry) and mouse (Just Works) in pairing mode | both paired in order (`BT:PAIR:START`, `PASSKEY`, `BT:ENCRYPT`, `BT:PAIR:PASS`, `BT:HID:*`), the keyboard with an authenticated key, four bond saves, `hello` typed through the keyboard arrives, the mouse moves the cursor and clicks |
| `--reboot` | the same, then the same image again with the peers pressing keys from the first moment | boot 2 loads two bonds before it is ready, never pairs, both peers reconnect by themselves and `lazy` typed during the boot arrives; `BT:READY ms=` and `BT:RECONNECT ms=` are reported (`BT:TIMING`) |
| `--refuse` | a keyboard that holds a key the host never issued | `BT:REFUSE reason=unbonded`, no connection, no source, no key at `inputd` |
| `--kill` | keyboard holds `a`; `btd` exits (crash-test build) | `inputd` saw `a` released before `init`'s restarted `btd` came up, the restart reattached, `zq` typed afterwards arrives, nothing is held at the end |

The LE modes (`judge_le.py`, peers in `modes_le.py`; the pairing list carries
`le:` entries for them) check the lines an LE connection must produce in
order, `BT:CONN ... role=le rpa=<0|1>`, `BT:PAIR:START`, `PASSKEY`,
`BT:ENCRYPT`, only then `BT:PAIR:PASS` and `BT:HID:*`, plus the same `inputd`
verdict:

| Mode | Peers | The verdict |
|---|---|---|
| `--le-pair` | LE keyboard (Passkey Entry, 65-octet Report Map read in pieces) and LE mouse (Just Works) in pairing mode | both paired over SMP and bonded, the keyboard with an authenticated key, `hello` and the mouse motion arrive; every connection under the device's own address (`rpa=0`) |
| `--le-reboot` | the same, then the same image again with the keyboard pressing keys from the first moment | boot 2 loads two bonds before it is ready, never pairs or discovers, both reconnect by themselves from the accept list (`rpa=0`) and `lazy` typed during the boot arrives; `BT:READY ms=` and `BT:RECONNECT ms=` are reported |
| `--le-rpa` | as `--le-reboot` with bonded devices advertising under a fresh resolvable private address each time | every reconnect is `rpa=1` (the host resolved it with the IRK), the keyboard also drops and returns once under another address: `lazygo` arrives |
| `--le-refuse` | a Just Works keyboard in the pairing list, and a keyboard with a key the host never issued | `BT:REFUSE reason=keyboard-unauthenticated` for the first, which gets no bond, no source and types nothing; the second is never connected |

`fakebt`'s own log (`shots/bt/fakebt.log`) is for reading, not for the verdict.
Its script (`fakebt/src/script.rs`) is `--at [armedN+|hidN+|mhidN+]MS:ACTION`:
type, key, hold, release, wake, move, drop; bonds persist with `--state`.
The LE peers are `--le-keyboard`, `--le-mouse` (in the keyboard and mouse
slots), `--le-jw-keyboard`, `--le-rpa`, `--le-early-notify` and
`--le-intruder` (`itype=` types on it).

## Radios that need firmware (B7)

`--fw KIND` makes `fakebt` play a vendor radio (`--radio intel-ax201|intel-ax211|rtl8822cu|rtl8761bu`,
`libs/btsim/src/radio.rs`) with the real part's USB ids, so `btd` picks the Intel or
Realtek loader of `libs/btfw`, reads the file the radio's own version answer names
from the image's firmware tree (the real linux-firmware files:
`run.py` fetches them with `tools/firmware/fetch.py --switch bt`, and the
`LAZYOS_BT=1` image build places them), downloads them, and then pairs and types
as `--pair` does. `modes_fw.py` holds the kinds and `judge_fw.py` the verdict:
`BT:FW:VENDOR`, the files asked for in order and read, `BT:FW:DONE` before
`BT:CTRL:PASS`, the re-enumerations the kind expects (`intel-recovery`: the
bootloader rejects the first image, `btd` sends the recovery reset, the dongle
leaves the bus and returns, `usbd` unbinds and rebinds it; `intel-reboot`: the
boot reset takes it off the bus and it returns running the image) and one bind
per enumeration. The simulated bootloader verifies no signature. `fakebt`'s
`--fw-reject N`, `--fw-reboot` and `--fw-refuse-fragment N` inject the faults; the
procedure for the real machines is in
[docs/agents/hardware.md](../../docs/agents/hardware.md), "Bluetooth controllers".
