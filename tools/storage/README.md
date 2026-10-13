# USB storage harness

End-to-end check of `/home` on a USB stick
([`docs/architecture/usb-storage.md`](../../docs/architecture/usb-storage.md)).

```bash
python tools/storage/run.py               # build, make the sticks, boot twice, judge
python tools/storage/run.py --no-build    # reuse target/lazyos.img
python tools/storage/run.py --no-other    # only the home stick
python tools/storage/test_judge.py        # the judge fails when it should
python tools/storage/judge.py shots/storage/boot1/serial.log --nonce N --other
python tools/storage/stick.py stick.img   # make a lazyhome stick and fsck it
```

The image is built with `LAZYOS_SERVICES=1 LAZYOS_USB=1 LAZYOS_RESET_OS=1`
and BusyBox as the console shell (`LAZYOS_BUSYBOX`, or
`target/abi/busybox/busybox` from `tools/abi/busybox.py`). QEMU gets the
image on virtio-blk, no home disk, and `qemu-xhci` with a `usb-storage`
stick (`stick.py`: MBR, one partition at 1 MiB, an ext2 `lazyhome` volume
from `tools/mkdisk --home-volume`) plus, unless `--no-other`, a second stick
labelled `otherdisk` that must stay unmounted.

1. **Boot 1**: wait for `INIT:HOME mounted`, log in on the console as
   `user`, write a nonce to `/home/user/usbnote`, read it back, `poweroff`;
   QEMU (`-no-shutdown`) stops after `power: filesystems synced`.
2. **Boot 2**: the volume must mount clean; read the nonce back, write
   `/home/user/second`, `poweroff`.
3. **Host**: `e2fsck -fn` on the stick's partition is clean; `debugfs` finds
   both files.

`judge.py` checks each boot's serial log: the sticks were served
(`USBD:MSC:DISK`), `/home` was mounted late exactly once and `init` saw it,
the login worked, the nonce came back as output, the shutdown synced, and
nothing failed (`USBD:FATAL|PANIC|MSC:FAIL|PORT:FAIL`, a kernel panic,
`power: sync failed`, an unclean mount on boot 2).

Under TCG (`--accel none`) a run takes about half an hour; output lands in
`shots/storage/`.

## FAT sticks at `/media`

```bash
python tools/storage/fat_run.py               # build, boot once, judge
python tools/storage/fat_run.py --no-build    # reuse target/lazyos.img
```

The same image and `/home` stick as above, plus a 64 MiB FAT32 stick (MBR,
label `MY STICK`, made by `cargo run -p fatvol --example fatstick`) that is
**not** attached at boot. The session logs in, plugs the stick in over QMP,
reads, writes, makes a directory and copies a file under
`/media/MY_STICK`, pulls the stick (the mount must go and the shell keep
answering), plugs it in again (`blockdev-add`: QEMU drops a `-drive` when its
device is removed) and reads what it wrote, then powers off. The host then
reads the stick with the same library (`fatstick cat`) and runs
`fsck.fat -n` from dosfstools through Docker when Docker is there. The judge
fails on a kernel panic, a failed final sync, a disk provider that hung, a
volume that did not mount twice or left the home stick mounted wrongly.
