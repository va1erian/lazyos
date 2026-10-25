#!/bin/bash
# Runs inside the tools/selfhost/Dockerfile image: unpack the rustc source
# from /dl into the /b volume, build rustc, rust-std and cargo for musl, copy
# the dist tarballs to /out. build.py is the driver.
set -euo pipefail
VERSION="$1"
SRC="/b/rustc-$VERSION-src"
# The tree is the release plus tools/selfhost/patches; a different patch set
# unpacks it again (tar keeps the release's mtimes, so only what a patch
# touches is rebuilt).
PATCHES="$(cat /cfg/patches/*.patch | tr -d '\r' | sha256sum | cut -c1-64)"
if [ "$(cat "$SRC/.lazyos-extracted" 2>/dev/null)" != "$PATCHES" ]; then
    rm -rf "$SRC"
    tar -C /b -xf "/dl/rustc-$VERSION-src.tar.xz"
    for p in /cfg/patches/*.patch; do
        tr -d '\r' < "$p" | patch -d "$SRC" -p1 --forward
    done
    echo "$PATCHES" > "$SRC/.lazyos-extracted"
fi
cp /cfg/bootstrap.toml "$SRC/bootstrap.toml"
cd "$SRC"
python3 x.py dist rustc rust-std cargo --jobs "$(nproc)"
for c in rustc rust-std cargo; do
    cp /b/build/dist/"$c-$VERSION"-x86_64-unknown-linux-musl.tar.xz /out/
done
