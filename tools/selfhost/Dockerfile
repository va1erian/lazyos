# The build machine for a LazyOS rustc: Alpine, so the build triple is
# already x86_64-unknown-linux-musl, the ABI LazyOS runs. LLVM is not taken
# from Alpine: rustc 1.99 bundles LLVM 23 and builds it (X86 only) with lld.
# Pinned by digest: the same commit builds with the same toolchain.
FROM alpine:3.24@sha256:be2656061b536c1883c4034790e21d0a09c5aaaf51968b89368a3c0371ea2c9a
RUN apk add --no-cache \
        bash binutils cmake curl file g++ git libgcc linux-headers make musl-dev \
        ninja-build patch perl pkgconf python3 tar xz zlib-dev zlib-static
# Alpine keeps ninja out of PATH (samurai is its default ninja).
ENV PATH="/usr/lib/ninja-build/bin:${PATH}"
