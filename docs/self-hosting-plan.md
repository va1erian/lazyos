# LazyOS self-hosting plan

The goal: **LazyOS builds LazyOS.** Rust code is written, compiled and run on a
LazyOS machine, and in the end the kernel, its services and the compiler
itself are built there. This plan restarts that work from the toolchain end:
first a working `rustc` on LazyOS, then the pieces a real build needs around it.

Status: **SH0 is built.** `tools/selfhost/build.py` cross-compiles rustc 1.99.1
in Docker, `LAZYOS_RUSTC=1` (`run_demo.py --rustc`) puts it in the image, and a
LazyOS console compiles and runs a `std` program with it. SH1 onward is plan.

## 1. Why the compiler is a Linux musl compiler

LazyOS runs unmodified `x86_64-unknown-linux-musl` programs: the kernel
implements the Linux syscall ABI (route B of [`rust-std.md`](rust-std.md),
[`linux-abi-plan.md`](linux-abi-plan.md)) and loads dynamically linked musl
programs through musl's own `ld-musl-x86_64.so.1` (issue #535). So a rustc
whose *host* triple is `x86_64-unknown-linux-musl` is a rustc for LazyOS, and
the programs it makes for that target run on LazyOS too. No Rust source is
patched and no new target triple is needed. A native target
(`x86_64-unknown-lazyos`, route A) stays possible later; nothing here depends
on it.

"Cross-compiling rustc" means building that compiler on another machine (the
developer's Linux, Windows or macOS box, in Docker) for the LazyOS host, the
way Rust's own CI builds its dist compilers.

## 2. SH0: the prototype (built)

### The cross build (`tools/selfhost/`)

| File | What it does |
|---|---|
| `Dockerfile` | The build machine: Alpine 3.24, whose native triple is already musl, with g++, cmake, ninja and python. |
| `bootstrap.toml` | x.py's configuration: host and target `x86_64-unknown-linux-musl`, in-tree LLVM 23 built for X86 only with a static libstdc++, `rust.lld = true` (so `rust-lld` ships), no debug info, `crt-static = false` (the compiler loads `librustc_driver` as a shared object). |
| `build-in-container.sh` | Unpacks the pinned source and runs `x.py dist rustc rust-std`. |
| `stage-in-container.sh` | Installs the two dist tarballs as a `/system` tree, strips the host programs, drops what a compile on LazyOS never runs (rustdoc, the debugger scripts, the wasm tools) and adds musl's loader and `libgcc_s.so.1`. |
| `build.py` | The driver: downloads the source release (pinned by SHA-256), builds the image, runs the cross build (cached: its tarballs in `target/selfhost/out`, its tree in the Docker volume `lazyos-rustc`), stages `target/selfhost/system`. `--upstream` stages the release's own musl-host compiler instead, in minutes; `--cpus N` caps the build container. |

```bash
python tools/selfhost/build.py              # cross build (half an hour the first time), then stage
python tools/selfhost/build.py --upstream   # the official musl-host build instead
python tools/run_demo.py --rustc            # build it if needed, embed it, boot
```

### In the image (`LAZYOS_RUSTC=1`)

`build_support/rustc_embed.rs` places the staged tree with `/system` as the
compiler's sysroot:

- `/system/bin/rustc` (`fhs::bin::RUSTC`), found by name from the shell;
- `/system/lib/librustc_driver-*.so`, which `rustc`'s `RPATH $ORIGIN/../lib`
  finds and from whose path rustc derives the sysroot `/system`;
- `/system/lib/rustlib` (`fhs::RUSTLIB`): the standard library's rlibs for
  `x86_64-unknown-linux-musl`, its self-contained CRT objects and `libc.a`,
  and `x86_64-unknown-linux-musl/bin/rust-lld`, the linker;
- musl's loader (also `libc.so`, which the compiler's `DT_NEEDED` names) and
  `libgcc_s.so.1` in `/system/lib`.

Everything under `/system/lib` is mode 0755, so `rust-lld` is executable. The
tree is several hundred MiB, so the switch adds 1 GiB to the default OS volume
(`os_disk::RUSTC_EXTRA`, mirrored in `tools/lazygui/osvolume.py`) and
`run_demo.py --rustc` uses the big-app profile (4 GiB of RAM, a 3 GiB quota).

### Compiling on LazyOS

There is no `cc` on LazyOS, so rustc calls `rust-lld` directly:

```sh
rustc -C linker=/system/lib/rustlib/x86_64-unknown-linux-musl/bin/rust-lld \
      -C linker-flavor=ld.lld hello.rs
./hello
```

(With the `ld.lld` flavor rustc passes `-flavor gnu` itself; the `gcc-ld/ld.lld`
wrapper adds it again and `rust-lld` refuses the second one.) The output is a
static PIE with the self-contained musl CRT, like any musl build.

`tools/screenshot/examples/rustc_console.json` is the check: `rustc -vV`, then a
program using `Vec`, iterators, `HashMap` and `println!` is written, compiled
and run, and the session waits for `HELLO:FROM:LAZYOS:55`.

```bash
LAZYOS_CLI=1 LAZYOS_RUSTC=1 LAZYOS_LIMIT_QUOTA_USER_MEMORY=3G cargo build
python tools/screenshot/qemu_session.py --image target/lazyos.img --memory 4G \
    --out shots/rustc --script tools/screenshot/examples/rustc_console.json
```

### Results (2026-10-25, WHPX, 4 GiB guest)

| Compiler | Staged tree | `rustc -vV` | Compile `hello.rs` | `hello` | Output |
|---|---|---|---|---|---|
| Our cross build (`build.py`) | 352 MiB | "built from a source tarball", LLVM 23.1.1 | 4.88 s | 1.3 MB | `HELLO:FROM:LAZYOS:55` |
| Official musl-host build (`--upstream`) | 667 MiB | LLVM 23.1.1 | 3.25 s | 4.9 MB | `HELLO:FROM:LAZYOS:55` |

The cross build took 29 minutes on 24 cores (Docker capped at 12 for most of
it): LLVM and lld, then the stage 1 and stage 2 compilers. Ours is half the
size because its LLVM has one target and its standard library no debug info
(hence the smaller program too). Why the official compiler compiles faster
was not measured; Rust's CI builds its compilers with more optimisation
settings than `bootstrap.toml` sets.

The first boot of our build failed with exit 127: unlike the official one,
its driver library needs `libz.so.1` (Alpine's LLVM build found the system
zlib) and names musl's libc `libc.musl-x86_64.so.1`, Alpine's name for it.
Staging now ships both and checks every `DT_NEEDED` of `rustc`, the driver and
`rust-lld` before the image is built.

What the prototype found missing in the kernel: only `membarrier` (324)
returns `ENOSYS`, which rustc's runtime tolerates. Threads, futexes, `mmap` of
the 200+ MiB driver library, `posix_spawn` of the linker, `/proc/self/exe` and
the temporary directory all worked as they are.

## 3. What follows

Each stage ends in a console or session check like SH0's, and kernel gaps it
finds get their own correctness and stress tests (AGENTS.md).

- **SH1: a default linker and cargo.** Ship a target spec default (or a
  `/system/etc` rustc config) so a plain `rustc hello.rs` links with
  `rust-lld`; cross-build cargo (`extended = true`, `tools = ["cargo"]`) and
  build a multi-crate workspace offline from vendored sources.
- **SH2: proc macros.** Proc-macro crates are shared objects rustc `dlopen`s;
  the dynamic loader path exists (#535), so `serde_derive` is the test.
- **SH3: LazyOS's own crates.** Add the `x86_64-unknown-none` standard library
  (and `rust-src` for `-Z build-std` on a nightly) and build `user/` programs
  and an xui app on LazyOS, then run them.
- **SH4: a C toolchain.** Crates with build scripts that call `cc` need a C
  compiler: clang (from the same LLVM) or zig, cross-built the same way.
- **SH5: the kernel and the image.** `cargo build` of the LazyOS workspace on
  LazyOS, producing an image that boots.
- **SH6: the compiler builds itself.** `x.py` on LazyOS (python, cmake, ninja
  and a C++ compiler on LazyOS first), the last step of self-hosting.

Performance work comes with it: where a compile on LazyOS spends its time is
not measured yet, and every later stage multiplies it.
