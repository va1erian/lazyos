#!/usr/bin/env bash
# Independent check of an image written by libs/ext2fs/examples/mkimage.rs
# (docs/filesystem-plan.md F2): e2fsck must be clean, and dumpe2fs/debugfs must
# show the superblock, modes, owners, manifest and file contents the library
# promised. The image is a bare ext2 volume, not a partitioned disk; for the
# build-made OS image see tools/ci/check_os_image.sh.
#
#   bash tools/ci/check_lib_image.sh IMAGE populated|updated UUID LABEL
#
# `populated` is a freshly formatted and populated volume; `updated` is the
# same volume closed, reopened and changed in place (a file shrunk, a subtree
# removed, files added). Both must be e2fsck-clean.
set -euo pipefail

image=${1:?image path}
stage=${2:?populated|updated}
want_uuid=${3:?uuid}
want_label=${4:?label}
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

fail() { echo "FAIL ($stage): $*" >&2; exit 1; }
dbg() { debugfs -R "$1" "$image" 2>/dev/null; }

# 1. The filesystem is consistent. -n: never modify; -f: check even if "clean".
e2fsck -fn "$image"

# 2. Superblock: what the formatter and the driver agree to write.
header=$(dumpe2fs -h "$image" 2>/dev/null)
grep -Eq '^Filesystem volume name: +'"$want_label"'$' <<<"$header" || fail "volume name"
got_uuid=$(sed -n 's/^Filesystem UUID: *//p' <<<"$header")
[ "$got_uuid" = "$want_uuid" ] || fail "uuid is $got_uuid, wanted $want_uuid"
grep -Eq '^Filesystem revision #: +1' <<<"$header" || fail "not revision 1"
grep -Eq '^Filesystem state: +clean$' <<<"$header" || fail "state not clean after flush"
features=$(sed -n 's/^Filesystem features: *//p' <<<"$header")
for feature in filetype sparse_super large_file; do
  grep -qw "$feature" <<<"$features" || fail "missing feature $feature ($features)"
done
for feature in has_journal extent 64bit huge_file dir_index meta_bg; do
  ! grep -qw "$feature" <<<"$features" || fail "unexpected feature $feature"
done

# 3. Modes and owners.
expect() { # path, octal mode (leading 0 optional), uid, gid
  local out
  out=$(dbg "stat $1")
  grep -Eq "Mode: +0?$2( |$)" <<<"$out" || { echo "$out"; fail "$1: wrong mode (wanted $2)"; }
  grep -Eq "User: +$3 +Group: +$4 " <<<"$out" || { echo "$out"; fail "$1: wrong owner (wanted $3:$4)"; }
}
expect / 0755 0 0
expect /lost+found 0700 0 0
expect /system 0755 0 0
expect /system/big.bin 0644 0 0
expect /system/.image-manifest 0644 0 0
expect /data/home/alice 0755 1000 1000
expect /data/tmp 1777 0 0

# 4. Contents. mkimage writes byte i of a file as (i*31 + seed) mod 256.
check_pattern() { # path, size, seed
  rm -f "$work/f"
  dbg "dump $1 $work/f" >/dev/null
  python3 - "$work/f" "$2" "$3" "$1" <<'PY'
import sys
path, size, seed, name = sys.argv[1], int(sys.argv[2]), int(sys.argv[3]), sys.argv[4]
data = open(path, "rb").read()
want = bytes((i * 31 + seed) & 255 for i in range(size))
if data != want:
    sys.exit(f"{name}: contents differ (got {len(data)} bytes, wanted {size})")
PY
}
listing=$(dbg "ls -p /system")
manifest=$(dbg "cat /system/.image-manifest")
if [ "$stage" = populated ]; then
  check_pattern /system/big.bin 300000 2
  check_pattern /system/bin/tool 20000 1
  expect /system/bin 0755 0 0
  expect /system/bin/tool 0755 0 0
  grep -q '^f /system/bin/tool$' <<<"$manifest" || fail "manifest lacks /system/bin/tool"
else
  check_pattern /system/big.bin 70000 3
  check_pattern /system/sbin/tool2 9000 4
  expect /system/sbin/tool2 0755 0 0
  expect /data/home/alice/notes.txt 0600 1000 1000
  [ "$(dbg 'cat /data/home/alice/notes.txt')" = "keep me" ] || fail "notes.txt contents"
  grep -q '^f /system/sbin/tool2$' <<<"$manifest" || fail "manifest lacks /system/sbin/tool2"
  ! grep -q '^f /system/bin/tool$' <<<"$manifest" || fail "manifest still lists /system/bin/tool"
  ! grep -q '/bin/' <<<"$listing" || fail "/system/bin survived remove_tree"
fi
# 5. Nothing but directories at the root (F3); `ls -p` prints
# /inode/mode/uid/gid/name/size/ and regular files are mode 100xxx.
root_files=$(dbg "ls -p /" | awk -F/ '$3 ~ /^100/ { print $6 }')
[ -z "$root_files" ] || fail "regular files at the root: $root_files"
echo "OK: $image ($stage) is e2fsck-clean with the expected modes, owners and contents"
