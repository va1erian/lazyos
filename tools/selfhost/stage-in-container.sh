#!/bin/bash
# Runs inside the tools/selfhost/Dockerfile image: install the rustc and
# rust-std dist tarballs found in /in into a /system-shaped tree in /out,
# stripped, with the shared libraries the compiler needs.
# Layout (sysroot = /system): bin/rustc, lib/librustc_driver-*.so,
# lib/rustlib/x86_64-unknown-linux-musl/{lib,bin}.
set -euo pipefail
rm -rf /tmp/stage /out/system && mkdir -p /tmp/stage /out
for t in /in/rustc-*-x86_64-unknown-linux-musl.tar.xz \
         /in/rust-std-*-x86_64-unknown-linux-musl.tar.xz; do
    tar -C /tmp -xf "$t"
    dir="/tmp/$(basename "$t" .tar.xz)"
    "$dir/install.sh" --prefix=/tmp/stage/system --disable-ldconfig >/dev/null
done
S=/tmp/stage/system
# The installer's bookkeeping, docs, debugger scripts and the tools a
# compile on LazyOS never runs are not part of the toolchain.
T="$S/lib/rustlib/x86_64-unknown-linux-musl/bin"
rm -rf "$S/lib/rustlib/"{components,rust-installer-version,uninstall.sh,manifest-*,etc} \
       "$S/lib/rustlib/install.log" "$S/share" "$S/etc" "$S/libexec" \
       "$S/bin/"{rustdoc,rust-gdb,rust-gdbgui,rust-lldb} \
       "$T/"{rust-objcopy,wasm-component-ld} "$T/gcc-ld/"{ld64.lld,lld-link,wasm-ld}
# Symbols of the host programs are much of their size; rlibs keep theirs
# (their metadata is what the compiler reads).
find "$S" -type f \( -name '*.so' -o -perm -u+x \) ! -name '*.rlib' ! -name '*.o' \
    -exec sh -c 'file -b "$1" | grep -q ELF && strip --strip-unneeded "$1"' _ {} \;
# The compiler's DT_NEEDED: musl's libc (the loader itself, under the name
# Rust's CI build uses and the one Alpine's does), libgcc_s for unwinding,
# and zlib, which LLVM links for compressed sections.
cp /lib/ld-musl-x86_64.so.1 "$S/lib/"
cp /lib/ld-musl-x86_64.so.1 "$S/lib/libc.so"
cp /lib/ld-musl-x86_64.so.1 "$S/lib/libc.musl-x86_64.so.1"
cp /usr/lib/libgcc_s.so.1 "$S/lib/"
cp -L /usr/lib/libz.so.1 "$S/lib/"
# Fail here, not on LazyOS, when a library the compiler needs is missing.
for f in "$S/bin/rustc" "$S"/lib/librustc_driver-*.so "$T/rust-lld"; do
    for need in $(readelf -d "$f" | sed -n 's/.*NEEDED.*\[\(.*\)\]/\1/p'); do
        [ -f "$S/lib/$need" ] || { echo "missing $need (needed by $f)" >&2; exit 1; }
    done
done
cp -r "$S" /out/system
du -sh /out/system
