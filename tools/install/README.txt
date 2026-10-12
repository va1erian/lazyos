LazyOS beside Linux: lazyos-install
===================================

This directory installs LazyOS onto two partitions of a disk that already
runs Linux, and makes LazyOS an entry of that Linux's GRUB menu. Linux keeps
booting as before. The full guide is docs/installing-beside-linux.md in the
LazyOS sources; the design is docs/grub-install-plan.md.

What the Linux side must provide (made with its own tools):

  * a UEFI boot (no BIOS/CSM), Secure Boot off;
  * GRUB installed and working, with an EFI System Partition (ESP);
  * two empty GPT partitions for LazyOS, ideally named "lazyos-system"
    (4 GiB recommended, 1 GiB minimum) and "lazyos-home" (any size).
    With gdisk or parted, set the names; then the installer finds them.

From Linux, with this stick mounted (say at /media/you/LAZYOS; desktops
mount FAT sticks without execute rights, so copy the program out first):

  cp /media/you/LAZYOS/install/lazyos-install /tmp/ && chmod +x /tmp/lazyos-install
  sudo /tmp/lazyos-install --stick /media/you/LAZYOS --dry-run   # writes nothing
  sudo /tmp/lazyos-install --stick /media/you/LAZYOS             # type the partition name back
  sudo grub-reboot LazyOS && sudo reboot                         # try LazyOS once

(Run from the stick itself, where it is executable, --stick can be left out.)

Later:

  sudo /tmp/lazyos-install update --stick ...   # a newer stick's LazyOS; keeps
                                                # your settings, apps and /home
  sudo /tmp/lazyos-install status
  sudo /tmp/lazyos-install uninstall            # removes the ESP files and the
                                                # GRUB entry; the partitions stay

It writes only: the two LazyOS partitions, EFI/lazyos/ on the ESP, and
/etc/grub.d/40_lazyos (or a marked block in GRUB's custom.cfg). If LazyOS
does not come up, power off: the next boot is Linux again. A hidden GRUB
menu shows when you hold Shift or press Esc at power-on.

40_lazyos.in is the GRUB entry the installer writes, for adding it by hand
(replace @ESP_UUID@ with the ESP's FAT serial, from `blkid`).
