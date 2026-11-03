# Bluetooth transport harness

[docs/bluetooth-plan.md](../../docs/bluetooth-plan.md) phase B2: `usbd` binds a
USB Bluetooth controller and serves it to `btd`
([docs/architecture/usb.md](../../docs/architecture/usb.md), "Bluetooth
controllers"). QEMU has had no Bluetooth emulation since `-bt` was removed, so
the controller is **`fakebt`**, a host program that serves a fake class
`E0/01/01` dongle over **usbredir** (`-chardev socket ... -device usb-redir`).
`fakebt` is Rust, not Python, because the controller it serves is a Rust
trait (`fakebt/src/controller.rs`, the one in the plan) that `libs/btsim`
implements too: swapping the stub for the simulator is the one line in
`fakebt/src/main.rs` that builds the controller. It needs QEMU built with
usbredir (`qemu-system-x86_64 -device help | grep usb-redir`; the Windows
build in `C:\Program Files\qemu` has it).

```bash
python tools/bt/run.py                # build (LAZYOS_SERVICES=1 LAZYOS_USB=1 LAZYOS_BT=1), boot, judge
python tools/bt/run.py --no-build     # reuse target/lazyos.img
python tools/bt/run.py --unplug       # pull the dongle out after the bring-up
python tools/bt/run.py --hotplug      # the dongle appears 20 s after the connection, not at boot
python tools/bt/test_judge.py         # the judge fails when it should
cargo test -p fakebt -p btusb -p btpolicy
python tools/run_demo.py --bt         # an interactive boot with the fake dongle plugged in
```

The verdict is the guest's serial log (`judge.py`): `USBD:BT:BIND` for
interface 0 only, with the dongle's descriptors byte for byte; `btd` running as
`_bt` with only `CAP_INPUT_SOURCE`; `BT:CTRL:PASS` carrying the version,
manufacturer and last two address octets the fake controller answered to
`HCI_Reset`, `Read_Local_Version` and `Read_BD_ADDR` (the full address must
never appear); and, with `--unplug`, `USBD:BT:UNBIND`, the `USBD:DETACH` of a
`bt` function, `USBD:BT:WITHDRAW` and `BT:CTRL:GONE`. `fakebt`'s own log
(`shots/bt/fakebt.log`) is for reading, not for the verdict.
