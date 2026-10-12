# Patches

`patches/icu/*.patch` and `patches/webkit/*.patch` (unified diffs, `patch -p1`)
are applied once, in name order, to the extracted tarball by
`toolenv.apply_patches` (the applied names are kept in `.lazyos-patched` in
the tree). Nothing is ever edited by hand.

**None are needed so far** for ICU 74.2 and WebKitGTK 2.54.1: musl and zig's
libc++ carried everything JavaScriptCore asks of them, and the three build
problems met were solved with CMake flags (`webkit.py`, `README.md`). A fix
that is not a flag goes here, one file per change, with the reason in the
patch header.
