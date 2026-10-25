#!/bin/bash
# Runs inside the tools/selfhost/Dockerfile image: unpack the rustc source
# from /dl into the /b volume, build rustc + rust-std for musl, copy the
# dist tarballs to /out. build_rustc.py is the driver.
set -euo pipefail
VERSION="$1"
SRC="/b/rustc-$VERSION-src"
if [ ! -f "$SRC/.lazyos-extracted" ]; then
    rm -rf "$SRC"
    tar -C /b -xf "/dl/rustc-$VERSION-src.tar.xz"
    touch "$SRC/.lazyos-extracted"
fi
cp /cfg/bootstrap.toml "$SRC/bootstrap.toml"
cd "$SRC"
python3 x.py dist rustc rust-std --jobs "$(nproc)"
cp build/dist/rustc-"$VERSION"-x86_64-unknown-linux-musl.tar.xz \
   build/dist/rust-std-"$VERSION"-x86_64-unknown-linux-musl.tar.xz /out/
