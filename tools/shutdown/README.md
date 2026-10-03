# Shutdown harness

End-to-end check of the orderly shutdown and reboot ([`docs/shutdown.md`](../../docs/shutdown.md)).

```bash
python tools/shutdown/run.py              # build the desktop image, boot twice, judge
python tools/shutdown/run.py --no-build   # reuse target/lazyos.img
python tools/shutdown/test_judge.py       # the judge fails when it should
python tools/shutdown/judge.py shots/shutdown/poweroff/serial.log --mode poweroff
```

`run.py` boots the desktop image twice (no data disk: since F4 nothing lives
under `/data`):

1. The Terminal writes a nonce to the session account's home
   (`/home/<name>/shutdown.txt`, from the passwd `build.rs` embeds) and types
   `shutdown`; the shutting-down overlay is captured and QEMU powers off.
2. The Terminal reads the nonce back, then LazyShell's start menu
   "Restart..." is chosen and confirmed (`SHELL:POWER:CONFIRM`,
   `SHELL:POWER:REQUEST mode=1`; LazyShell must not be started again); the "Restarting..." overlay is captured and QEMU
   (run with `-no-reboot`) exits on the reset.

`judge.py` reads a serial log and checks the sequence the shutdown promises:
`init` armed the kernel watchdog; the phases ran in order (stopping, apps,
services, quiesced, power); `CONFD:STOP` (sync ok, `dir=/conf` on a desktop
boot), `LOGD:STOP` (chain verified, `persisted>0`) and, when `pkgd` ran,
`PKGD:STOP` (stopped through the lifecycle contract, before `confd`) came
inside the services phase; nothing was killed at a deadline or restarted after
the request; the kernel synced and did not fall back to a triple fault or a
halt. The second boot must also mount `/` clean, print the nonce and find the
first boot's records in `/logs/service.log`, checked again from the host after
QEMU exits (`cargo run -q -p ext2fs --example osread -- target/lazyos.img cat
/logs/service.log`; `/logs` is 0750 root).

Prerequisites: BusyBox at `target/abi/busybox/busybox` (`tools/abi/busybox.py`)
for the Terminal's `sh`, and the xui apps (`tools/xui/build.py`, which `run.py`
runs unless `--no-build`). Screenshots and logs land in `shots/shutdown/`.
