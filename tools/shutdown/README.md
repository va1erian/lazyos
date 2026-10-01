# Shutdown harness

End-to-end check of the orderly shutdown and reboot ([`docs/shutdown.md`](../../docs/shutdown.md)).

```bash
python tools/shutdown/run.py              # build the desktop image, boot twice, judge
python tools/shutdown/run.py --no-build   # reuse target/lazyos.img
python tools/shutdown/test_judge.py       # the judge fails when it should
python tools/shutdown/judge.py shots/shutdown/poweroff/serial.log --mode poweroff
```

`run.py` boots the desktop image twice on one fresh ext2 data disk
(`target/shutdown-data.img`):

1. The Terminal writes a nonce to `/data/tmp/shutdown.txt` and types
   `shutdown`; the shutting-down overlay is captured and QEMU powers off.
2. The Terminal reads the nonce back, then the desktop menu's "Restart..." is
   chosen and confirmed; the "Restarting..." overlay is captured and QEMU
   (run with `-no-reboot`) exits on the reset.

`judge.py` reads a serial log and checks the sequence the shutdown promises:
`init` armed the kernel watchdog; the phases ran in order (stopping, apps,
services, quiesced, power); `CONFD:STOP` (sync ok) and `LOGD:STOP` (chain
verified) came inside the services phase; nothing was killed at a deadline or
restarted after the request; the kernel synced and did not fall back to a
triple fault or a halt. The second boot must also mount `/data` clean and
print the nonce.

Prerequisites: BusyBox at `target/abi/busybox/busybox` (`tools/abi/busybox.py`)
for the Terminal's `sh`, and the xui apps (`tools/xui/build.py`, which `run.py`
runs unless `--no-build`). Screenshots and logs land in `shots/shutdown/`.
