#!/bin/sh
# Collect the facts about a PC that LazyOS needs, from the Linux already on it.
#
# Run it as root on the machine's own Linux, before its first LazyOS boot
# (docs/kabylake-box-plan.md step B0, docs/compat/README.md):
#
#     sudo sh tools/boot/survey_linux_box.sh [output-dir]
#
# It writes one text file per command into <output-dir> (default
# ./survey-<hostname>-<date>) and a .tar.gz next to it. Nothing is changed on
# the machine: every command is read-only, and a missing tool or a refused
# command only leaves a note in its file. Commit the directory (or the parts
# worth keeping) as docs/compat/<machine>/ -- but first REDACT the machine
# identifiers it holds (DMI serials and UUIDs, disk serials, filesystem UUIDs,
# MAC addresses, lspci serial numbers; docs/compat/kabylake/B0.md step 1) and
# do not commit the .tar.gz.
#
# Packages that add the optional tools on Debian/Ubuntu:
#     apt install pciutils usbutils dmidecode efibootmgr ethtool acpica-tools \
#                 cpuid mokutil util-linux
#
# POSIX sh on purpose (no bashisms): it also runs from a minimal live image.

set -u

stamp=$(date +%Y%m%d-%H%M)
host=$(hostname 2>/dev/null || echo box)
out=${1:-./survey-$host-$stamp}
mkdir -p "$out" || exit 1

if [ "$(id -u)" != 0 ]; then
    echo "note: not root; lspci -vvv, dmidecode and /proc/iomem will be incomplete" >&2
fi

# run <file> <command...>: stdout and stderr of the command go to $out/<file>.
run() {
    name=$1
    shift
    {
        echo "# $*"
        if command -v "$1" >/dev/null 2>&1; then
            "$@" 2>&1
            echo "# exit $?"
        else
            echo "# not installed: $1"
        fi
    } >"$out/$name"
}

# sh -c variant for pipelines and globs.
runsh() {
    name=$1
    shift
    {
        echo "# $*"
        sh -c "$*" 2>&1
        echo "# exit $?"
    } >"$out/$name"
}

# Identity ------------------------------------------------------------------
run 00-uname.txt uname -a
runsh 00-os-release.txt 'cat /etc/os-release'
run 01-dmidecode.txt dmidecode
runsh 01-dmi-sysfs.txt 'for f in /sys/class/dmi/id/*; do [ -f "$f" ] && [ -r "$f" ] && printf "%s: " "$(basename $f)" && cat "$f"; done'

# Firmware mode, Secure Boot, boot entries ---------------------------------
runsh 02-firmware-mode.txt '[ -d /sys/firmware/efi ] && echo "UEFI (efivars present)" || echo "legacy BIOS (no /sys/firmware/efi)"; ls /sys/firmware/efi 2>&1; cat /sys/firmware/efi/fw_platform_size 2>&1'
run 02-secureboot.txt mokutil --sb-state
run 02-efibootmgr.txt efibootmgr -v

# CPU and memory --------------------------------------------------------------
runsh 03-cpuinfo.txt 'cat /proc/cpuinfo'
run 03-lscpu.txt lscpu
run 03-cpuid-leaf15-16.txt cpuid -1 -r
runsh 03-tsc-and-clock.txt 'dmesg | grep -i -E "tsc|hpet|clocksource|lapic|apic timer|pmtmr|mwait|c-state|intel_idle"'
runsh 03-meminfo.txt 'cat /proc/meminfo | head -5; echo; cat /proc/iomem'
runsh 03-ioports.txt 'cat /proc/ioports'
runsh 03-interrupts.txt 'cat /proc/interrupts'
runsh 03-e820.txt 'dmesg | grep -i -E "e820|BIOS-provided|efi: |Memory:"'

# PCI -------------------------------------------------------------------------
run 04-lspci-nn.txt lspci -nn
run 04-lspci-tree.txt lspci -tvnn
run 04-lspci-vvv.txt lspci -vvnn -D
run 04-lspci-xxx.txt lspci -xxx -D
runsh 04-iommu-groups.txt 'for g in /sys/kernel/iommu_groups/*; do echo "group $(basename $g):"; for d in $g/devices/*; do printf "  "; lspci -nns "$(basename $d)" 2>/dev/null || basename $d; done; done'
runsh 04-acpi-tables.txt 'ls -l /sys/firmware/acpi/tables/ /sys/firmware/acpi/tables/dynamic 2>&1; dmesg | grep -i -E "ACPI: (RSDP|XSDT|FACP|APIC|HPET|DMAR|MCFG)"'

# USB -----------------------------------------------------------------------------
run 05-lsusb-tree.txt lsusb -t
run 05-lsusb.txt lsusb -v
runsh 05-xhci-dmesg.txt 'dmesg | grep -i -E "xhci|usb |usbhid|hid-|input:"'
runsh 05-input-devices.txt 'cat /proc/bus/input/devices'

# Storage -----------------------------------------------------------------------------
run 06-lsblk.txt lsblk -o NAME,SIZE,TYPE,TRAN,MODEL,SERIAL,FSTYPE,LABEL,UUID,PTTYPE,PHY-SEC,LOG-SEC,ROTA,MOUNTPOINT
runsh 06-ata-dmesg.txt 'dmesg | grep -i -E "ahci|ata[0-9]|scsi|sd[a-z]|nvme|sata"'
runsh 06-partition-tables.txt 'for d in /dev/sd? /dev/nvme?n?; do [ -b "$d" ] && fdisk -l "$d" 2>&1; done'

# GRUB and the ESP (docs/grub-install-plan.md I0) ---------------------------------
# Read-only: what an installer beside this Linux would have to register with.
runsh 06-grub-version.txt 'grub-install --version 2>&1 || grub2-install --version 2>&1'
runsh 06-grub-default.txt 'cat /etc/default/grub 2>&1; echo; ls -l /etc/grub.d 2>&1'
runsh 06-grub-boot-fs.txt 'findmnt -no SOURCE,FSTYPE,TARGET /boot 2>&1; findmnt -no SOURCE,FSTYPE,TARGET /boot/efi 2>&1; findmnt -no SOURCE,FSTYPE,TARGET / 2>&1'
runsh 06-esp-tree.txt 'for e in /boot/efi /efi /boot; do [ -d "$e/EFI" ] && { echo "== $e"; df -h "$e"; find "$e/EFI" -maxdepth 3 | sort; }; done'
runsh 06-esp-grub-stubs.txt 'for f in /boot/efi/EFI/*/grub.cfg /efi/EFI/*/grub.cfg; do [ -f "$f" ] && { echo "== $f"; cat "$f"; }; done'
runsh 06-grub-cfg-custom.txt 'for f in /boot/grub/grub.cfg /boot/grub2/grub.cfg; do [ -f "$f" ] && { echo "== $f"; grep -n -E "custom.cfg|menuentry |submenu |chainloader|set default|timeout" "$f" | head -60; }; done; ls -l /boot/grub/custom.cfg /boot/grub2/custom.cfg 2>&1'
run 06-gdisk.txt sgdisk -p /dev/sda
runsh 06-blkid.txt 'blkid 2>&1'

# Graphics and display ------------------------------------------------------------
runsh 07-fb.txt 'for f in /sys/class/graphics/fb*; do echo "== $f"; for a in name virtual_size stride bits_per_pixel modes; do printf "%s: " $a; cat $f/$a 2>&1; done; done'
runsh 07-drm.txt 'for c in /sys/class/drm/card*-*; do echo "== $c"; cat $c/status 2>&1; cat $c/modes 2>&1 | head -12; done'
runsh 07-gpu-dmesg.txt 'dmesg | grep -i -E "i915|drm|efifb|simpledrm|fb0|framebuffer|vgaarb|BAR"'

# Audio ------------------------------------------------------------------------------
runsh 08-asound-cards.txt 'cat /proc/asound/cards; echo; ls /proc/asound; for c in /proc/asound/card*/codec#*; do echo "== $c"; head -30 "$c"; done'
runsh 08-snd-dmesg.txt 'dmesg | grep -i -E "snd|hda|audio|codec"'

# Network ------------------------------------------------------------------------------
runsh 09-ip-link.txt 'ip -d link show; ip addr show'
runsh 09-ethtool.txt 'for i in $(ls /sys/class/net | grep -v "^lo$"); do echo "== $i"; ethtool -i $i 2>&1; ethtool $i 2>&1 | head -30; done'
runsh 09-nic-dmesg.txt 'dmesg | grep -i -E "r8169|rtl_nic|realtek|igc|e1000|eth[0-9]|enp|firmware|iwlwifi|rtw|link is"'
runsh 09-nic-mac-and-bars.txt 'for i in $(ls /sys/class/net | grep -v "^lo$"); do echo "$i $(cat /sys/class/net/$i/address 2>&1) $(readlink /sys/class/net/$i/device 2>&1)"; done'

# Power and thermal -------------------------------------------------------------------------
runsh 10-power-button.txt 'dmesg | grep -i -E "power button|sleep button|acpi.*(button|pm|s5|sci)|wdt|iTCO|watchdog"'
runsh 10-thermal.txt 'for z in /sys/class/thermal/thermal_zone*; do echo "$z $(cat $z/type 2>&1) $(cat $z/temp 2>&1)"; done'

# The whole kernel log, last, in case a grep above missed the interesting line --
run 99-dmesg.txt dmesg
runsh 99-loaded-modules.txt 'lsmod'

{
    echo "survey by tools/boot/survey_linux_box.sh on $host at $stamp"
    echo "run as uid $(id -u)"
    ls -l "$out"
} >"$out/README.txt"

tar -czf "$out.tar.gz" -C "$(dirname "$out")" "$(basename "$out")" 2>/dev/null &&
    echo "wrote $out and $out.tar.gz"
