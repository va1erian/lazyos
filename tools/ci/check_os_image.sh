#!/usr/bin/env bash
# Independent check of the OS volume in target/lazyos.img (docs/filesystem-plan.md
# F2): e2fsck must be clean, and debugfs must show the modes, owners and the
# manifest the build promises. `fresh` records the UUID; `updated` checks that
# an in-place update kept it.
#
#   bash tools/ci/check_os_image.sh target/lazyos.img fresh|updated
set -euo pipefail

image=${1:?image path}
stage=${2:?fresh|updated}
os_start_lba=131072 # 64 MiB: where the OS volume starts, fixed by the build
part=$(mktemp)
trap 'rm -f "$part"' EXIT

# MBR entry 3 (offset 0x1DE): type 0x83 at the fixed start.
read -r kind start < <(python3 - "$image" <<'PY'
import struct, sys
mbr = open(sys.argv[1], "rb").read(512)
entry = mbr[0x1BE + 32:0x1BE + 48]
print(entry[4], struct.unpack("<I", entry[8:12])[0])
PY
)
[ "$kind" = 131 ] || { echo "MBR entry 3 type is $kind, not 0x83" >&2; exit 1; }
[ "$start" = "$os_start_lba" ] || { echo "MBR entry 3 starts at $start" >&2; exit 1; }

dd if="$image" of="$part" bs=512 skip="$os_start_lba" status=none
e2fsck -fn "$part"

stat() { debugfs -R "stat $1" "$part" 2>/dev/null; }
expect() { # path, mode regex, uid
  local out
  out=$(stat "$1")
  grep -Eq "Mode: +$2" <<<"$out" || { echo "$1: wrong mode"; echo "$out"; exit 1; }
  grep -Eq "User: +$3 " <<<"$out" || { echo "$1: wrong owner"; echo "$out"; exit 1; }
}
expect /data/tmp 1777 0
expect /data/home/alice 0755 1000
expect /system 0755 0
for dir in /boot /home /transient /apps /conf /logs /data; do expect "$dir" 0755 0; done
expect /HELLO.ELF 0755 0
expect /HELLO.TXT 0644 0
expect /system/.image-manifest 0644 0
debugfs -R "cat /system/.image-manifest" "$part" 2>/dev/null | grep -q "^f /HELLO.ELF$"

uuid=$(dumpe2fs -h "$part" 2>/dev/null | sed -n 's/^Filesystem UUID: *//p')
echo "OS volume UUID $uuid ($stage)"
if [ "$stage" = fresh ]; then
  echo "$uuid" > "$(dirname "$image")/os-uuid.txt"
else
  [ "$uuid" = "$(cat "$(dirname "$image")/os-uuid.txt")" ] || { echo "UUID changed on update" >&2; exit 1; }
fi
