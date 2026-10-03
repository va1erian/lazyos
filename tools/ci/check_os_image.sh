#!/usr/bin/env bash
# Independent check of the OS volume in target/lazyos.img (docs/filesystem-plan.md
# F2/F3): e2fsck must be clean, and debugfs must show the modes, owners and the
# manifest the build promises: programs at 0755 in /system/bin, data at 0644,
# and no regular file at the root. `fresh` records the UUID; `updated` checks
# that an in-place update kept it.
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
expect /system 0755 0
for dir in /boot /home /transient /apps /docs/apps /data \
           /system/bin /system/etc /system/share /system/packages; do
  expect "$dir" 0755 0
done
# F4 (issue #508): service state at its place, private homes, nothing seeded
# under /data.
expect /conf 0700 0
expect /conf/svc 0700 0
expect /logs 0750 0
expect /home/admin 0700 0
expect /home/user 0700 1000
if stat /data/tmp | grep -q "Mode:"; then echo "/data/tmp is still seeded"; exit 1; fi
expect /system/bin/hello 0755 0
expect /system/share/samples/hello.txt 0644 0
expect /system/.image-manifest 0644 0
debugfs -R "cat /system/.image-manifest" "$part" 2>/dev/null | grep -q "^f /system/bin/hello$"

# Every program is 0755 root, and nothing but directories sits at the root
# (`ls -p` prints /inode/mode/uid/gid/name/size/, regular files are 100xxx,
# and ends with an empty line, which is no entry).
debugfs -R "ls -p /system/bin" "$part" 2>/dev/null | awk -F/ '
  NF < 6 || $6 == "." || $6 == ".." { next }
  $3 != "100755" { print "/system/bin/" $6 ": mode " $3; bad = 1 }
  $4 != "0" { print "/system/bin/" $6 ": owner " $4; bad = 1 }
  END { exit bad }'
root_files=$(debugfs -R "ls -p /" "$part" 2>/dev/null | awk -F/ '$3 ~ /^100/ { print $6 }')
[ -z "$root_files" ] || { echo "regular files at the root: $root_files" >&2; exit 1; }


uuid=$(dumpe2fs -h "$part" 2>/dev/null | sed -n 's/^Filesystem UUID: *//p')
echo "OS volume UUID $uuid ($stage)"
if [ "$stage" = fresh ]; then
  echo "$uuid" > "$(dirname "$image")/os-uuid.txt"
else
  [ "$uuid" = "$(cat "$(dirname "$image")/os-uuid.txt")" ] || { echo "UUID changed on update" >&2; exit 1; }
fi
