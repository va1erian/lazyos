#!/usr/bin/env bash
# Run a LazyOS image under QEMU/KVM with the Kaby Lake box's RTL8111H passed
# through by VFIO and no emulated NIC (docs/rtl8168-driver-plan.md R3).
# Linux only, as root, on the box itself; Linux keeps the Wi-Fi.
#
#   sudo tools/net/vfio_box.sh [--bdf 0000:02:00.0] [--image target/lazyos.img] [--dry-run] [-- extra qemu args]
#
# The NIC is unbound from r8169 and given to vfio-pci for the run, and given
# back on exit (also on ^C). Caveat: r8169 already set the chip up, so a pass
# here tests the rings, interrupts and MAC, not the PHY's cold start; that
# needs bare metal (plan section 5).
set -euo pipefail

BDF=0000:02:00.0
IMAGE=target/lazyos.img
DRY=0
while [ $# -gt 0 ]; do
  case "$1" in
    --bdf) BDF=$2; shift 2 ;;
    --image) IMAGE=$2; shift 2 ;;
    --dry-run) DRY=1; shift ;;
    --) shift; break ;;
    *) echo "unknown option $1" >&2; exit 2 ;;
  esac
done

run() { echo "+ $*" >&2; [ "$DRY" = 1 ] || "$@"; }
dev=/sys/bus/pci/devices/$BDF

[ "$DRY" = 1 ] || [ "$(id -u)" = 0 ] || { echo "run as root" >&2; exit 1; }
[ -d "$dev" ] || { echo "no such PCI function: $BDF" >&2; exit 1; }
[ -f "$IMAGE" ] || { echo "no image: $IMAGE (build with LAZYOS_NETD=1)" >&2; exit 1; }
ids=$(cat "$dev/vendor" "$dev/device" | tr '\n' ' ')
[ "$ids" = "0x10ec 0x8168 " ] || { echo "$BDF is $ids, not a 10ec:8168" >&2; exit 1; }
[ -d "$dev/iommu_group" ] || { echo "no IOMMU group for $BDF (is VT-d on? intel_iommu=on)" >&2; exit 1; }
group=$(basename "$(readlink "$dev/iommu_group")")
others=$(ls "/sys/kernel/iommu_groups/$group/devices" | grep -vc "^${BDF}\$" || true)
[ "$others" = 0 ] || { echo "IOMMU group $group has $others other function(s); refusing" >&2; exit 1; }

orig=$(basename "$(readlink "$dev/driver" 2>/dev/null || true)")
restore() {
  echo "giving $BDF back to ${orig:-nobody}" >&2
  if [ "$DRY" != 1 ]; then
    echo > "$dev/driver_override" || true
    [ -e "$dev/driver" ] && echo "$BDF" > "$dev/driver/unbind" || true
    [ -n "$orig" ] && echo "$BDF" > "/sys/bus/pci/drivers/$orig/bind" || true
  fi
}
trap restore EXIT

run modprobe vfio-pci
if [ -n "$orig" ]; then
  [ "$DRY" = 1 ] || echo "$BDF" > "$dev/driver/unbind"
fi
[ "$DRY" = 1 ] || echo vfio-pci > "$dev/driver_override"
[ "$DRY" = 1 ] || echo "$BDF" > /sys/bus/pci/drivers_probe

# q35 for PCIe like the box; the image's own disk is the boot disk. The serial
# log is the evidence: NETDRV:RTL8168, NETDRV:REGS, NETDRV:IRQ:Msi.
run qemu-system-x86_64 -machine q35 -enable-kvm -cpu host -m 2G \
  -drive "file=$IMAGE,format=raw,if=virtio" \
  -device "vfio-pci,host=$BDF" -nic none \
  -display none -serial mon:stdio "$@"
