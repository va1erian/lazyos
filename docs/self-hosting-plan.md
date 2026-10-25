# LazyOS self-hosting plan

The goal: **LazyOS builds LazyOS.** Rust code is written, compiled and run on a
LazyOS machine, and in the end the kernel, its services and the compiler
itself are built there. This plan restarts that work from the toolchain end:
first a working `rustc` on LazyOS, then the pieces a real build needs around it.

Status: **SH0 and SH1 are built.** `tools/selfhost/build.py` cross-compiles
rustc, cargo and rustdoc 1.99.1 in Docker, `LAZYOS_RUSTC=1` (`run_demo.py
--rustc`) puts them in the image, and a LazyOS console builds, tests and runs a
cargo workspace with them. SH2 onward is plan.

## 1. Why the compiler is a Linux musl compiler

LazyOS runs unmodified `x86_64-unknown-linux-musl` programs: the kernel
implements the Linux syscall ABI (route B of [`rust-std.md`](rust-std.md),
[`linux-abi-plan.md`](linux-abi-plan.md)) and loads dynamically linked musl
programs through musl's own `ld-musl-x86_64.so.1` (issue #535). So a rustc
whose *host* triple is `x86_64-unknown-linux-musl` is a rustc for LazyOS, and
the programs it makes for that target run on LazyOS too. No new target triple
is needed; one small patch changes the musl target's default linker (SH1). A native target
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
| `build-in-container.sh` | Unpacks the pinned source, applies `patches/`, and runs `x.py dist rustc rust-std cargo` (the build directory, `/b/build`, is outside the source tree, so a new patch rebuilds only what it touches). |
| `stage-in-container.sh` | Installs the dist tarballs as a `/system` tree, strips the host programs, drops what a compile on LazyOS never runs (the debugger scripts, the wasm tools), adds musl's loader, `libgcc_s.so.1` and `libz.so.1`, and fails when a program's `DT_NEEDED` library is missing. |
| `patches/` | Changes to the Rust source, one file each (SH1's default linker). |
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

There is no `cc` on LazyOS, so rustc calls `rust-lld` directly. SH0's
compiler needed the flags below; since SH1 they are its default:

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

## 3. SH1: cargo and a default linker (built)

### Cargo

`bootstrap.toml` builds cargo and rustdoc too (`extended = true`,
`tools = ["cargo", "rustdoc"]`). `cargo-native-static = true` builds cargo
with vendored OpenSSL, curl and libgit2, so it needs nothing beyond musl's
libc. `rustdoc` ships because `cargo test` runs it for doctests. Both go in
`/system/bin` (`fhs::bin::CARGO`, `fhs::bin::RUSTDOC`).

### A default linker

Plain `rustc hello.rs` and `cargo build` now link without flags. The patch
`tools/selfhost/patches/0001-musl-links-with-rust-lld.patch` makes the
`x86_64-unknown-linux-musl` target default to `rust-lld` with the `ld.lld`
flavor, as `x86_64-unknown-none` does. rustc puts the sysroot's tool directory
(`/system/lib/rustlib/x86_64-unknown-linux-musl/bin`) on the linker's `PATH`,
so the bare name is found. The build machine has a C compiler and links
through it: `bootstrap.toml` sets the target's `linker = "gcc"`. It must be
`gcc`, not `cc`: rustc infers a C-compiler flavor from names like `gcc` and
`clang` only, and would otherwise pass `cc` raw linker arguments.

An environment variable or a cargo config file would cover cargo only (LazyOS
has no system-wide shell profile; a `/.cargo/config.toml` would reach every
project, since cargo reads the config of each ancestor directory). A plain
`rustc` reads neither, so the default belongs in the target.

`--upstream` stages Rust's own compiler, which lacks the patch: on LazyOS it
links only with `-C linker=rust-lld` (or cargo's
`CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER=rust-lld`).

### The sample and the check

`tools/selfhost/cargo-hello` ships at `/system/share/samples/cargo-hello`
(`fhs::share::CARGO_SAMPLE`): a workspace of a library (`greet`, with two
unit tests, a doctest and a sum computed on scoped threads) and a program
(`hello`) that uses it through a path dependency. Its `Cargo.lock` is
committed, so it builds with `--offline`.

`tools/screenshot/examples/cargo_console.json` checks, each step gated on its
success:

1. `cargo -V`;
2. `rustc p.rs && ./p` with no linker flags (`plain rustc 42`);
3. a copy of the sample, then `cargo test --offline` (exit 0: unit tests and
   the doctest);
4. `cargo run --release --offline`
   (`Hello, LazyOS from cargo: sum 5050 on 4 threads, 9 distinct words`);
5. `cargo new --vcs none fresh && cd fresh && cargo run --offline`
   (`Hello, world!`).

```bash
python tools/selfhost/build.py
LAZYOS_CLI=1 LAZYOS_RUSTC=1 LAZYOS_LIMIT_QUOTA_USER_MEMORY=3G cargo build
python tools/screenshot/qemu_session.py --image target/lazyos.img --memory 4G \
    --out shots/cargo --script tools/screenshot/examples/cargo_console.json
```

### Results (2026-10-25, WHPX, 4 GiB guest)

| Step on LazyOS | Time |
|---|---|
| `cargo test` from clean (2 crates, test profile, then the doctest) | 11.8 s to build, doctests 3.4 s |
| `cargo run --release` from clean | 8.4 s |
| `cargo new` + `cargo run` | 2.9 s |

The staged tree is 399 MiB (cargo 36 MB, rustdoc 12 MB). The cross build of all
three took 32 minutes from clean on 12 of 24 cores; adding rustdoc to a built
tree took 2.5 minutes.

### Found: `execve` leaves the other threads running

`cargo run` replaces cargo with the program by `execve` while cargo still has
helper threads. Linux ends every other thread of the process first. LazyOS's
`sys_execve` (`kernel/src/process/linux/procctl.rs`) switches only the calling
thread to the new image; the others keep running the old program in the old
address space. Each `cargo run` leaves one such thread, which the console
multiplexer shows as a stray `thread` pane, and which holds a task slot. Running
the program directly, `cargo build`, `cargo test` and `rustdoc --test` leave
none. The fix is the kernel's own task (Linux's de-thread step, with
correctness and stress tests); until then the session passes but the leak is
visible in its screenshots.

## 4. What follows

Each stage ends in a console or session check like SH0's and SH1's, and kernel gaps it
finds get their own correctness and stress tests (AGENTS.md).

- **SH1.5: crates.io.** Fetch dependencies over the network (cargo's HTTPS
  through the vendored OpenSSL and the image's CA bundle), or from a vendored
  directory for offline images.
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
