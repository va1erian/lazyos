# Bluetooth harness

[docs/bluetooth-plan.md](../../docs/bluetooth-plan.md) phases B2 and B3: `usbd`
binds a USB Bluetooth controller and serves it to `btd`, which runs the BR/EDR
HID host ([docs/architecture/usb.md](../../docs/architecture/usb.md),
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
python tools/bt/test_judge.py         # the judges fail when they should
cargo test -p fakebt -p btusb -p btpolicy -p bthost -p btsdp -p bthid
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

`fakebt`'s own log (`shots/bt/fakebt.log`) is for reading, not for the verdict.
Its script (`fakebt/src/script.rs`) is `--at [armedN+|hidN+|mhidN+]MS:ACTION`:
type, key, hold, release, wake, move, drop; bonds persist with `--state`.
