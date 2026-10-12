# Patches

`patches/icu/*.patch` and `patches/webkit/*.patch` (unified diffs, `patch -p1`)
are applied once, in name order, to the extracted tarball by
`toolenv.apply_patches` (the applied names are kept in `.lazyos-patched` in
the tree). Nothing is ever edited by hand.

**ICU 74.2 needs none.** The C-loop `jsc` needed none either: musl and zig's
libc++ carried everything JavaScriptCore asks of them, and the three build
problems met were solved with CMake flags (`webkit.py`, `README.md`). The JIT
build (`build.py --jit`) needs two, both in WebKitGTK 2.54.1 as released and
neither LazyOS-specific in kind:

* `0001-inline-cache-compiler-include-ccallhelpers.patch`: with `ENABLE_JIT=ON`
  `InlineCacheCompiler.h` returns `CCallHelpers::Jump` by value from static
  members but only forward-declares the class, so a translation unit that
  reaches it first (`llint/LLIntOffsetsExtractor.cpp`) does not compile.
* `0002-macroassembler-x86-64-avx-needs-os-support.patch`: the JIT enables AVX
  from `CPUID.1:ECX` bit 28 alone and emits VEX instructions (`vmovsd`, ...),
  which are `#UD` unless the OS set `CR4.OSXSAVE` and `XCR0` (Intel's SDM says to
  check `OSXSAVE` and `XGETBV` first). LazyOS enables no AVX state, and a QEMU
  guest on WHPX or KVM reports AVX, so every optimizing tier ended in `SIGILL`
  (`c4 c1 7b 11 43 50`, `vmovsd [r11+0x50], xmm0`, in the code pool). The
  patch gates AVX and AVX2 on the OS having enabled them; when LazyOS turns
  AVX state on, JSC uses it with no further change.

A fix that is not a flag goes here, one file per change, with the reason in
the patch header.
