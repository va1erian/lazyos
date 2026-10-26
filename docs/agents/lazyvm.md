# LazyVM on LazyOS

Companion to [`AGENTS.md`](../../AGENTS.md); covers: LazyVM, the Java 8
bytecode VM that runs on LazyOS and Windows, and the LazyOS-side work it
needs: the `LAZYOS_LAZYVM` build switch and the test hook (LOS-01, below); the
display grant, the `midlc` Java backend and app packaging come later (LOS-02 to
LOS-04).

LazyVM lives in its own repository (`lazyvm`). Its plan is `docs/plan.md` and
`docs/plan-2.md` there; the LazyOS-side items are `LOS-01` to `LOS-04` at the
end of `plan-2.md`. On LazyOS it is a static `x86_64-unknown-linux-musl` `std`
program run through the Linux ABI, like `xui-app`: windows through xuid's
`os.lazy.display.v1` client mode, Messenger through `lazyos-sys`, and one
`wait` over `WAIT_CALLS`, endpoints and an epoll `WAIT_FD` for its event loop
([`messenger.md`](../messenger.md) sections 6, 7 and 15,
[`architecture/call-completion.md`](../architecture/call-completion.md),
[`architecture/endpoint-fd.md`](../architecture/endpoint-fd.md)).

## The build switch (`LAZYOS_LAZYVM=1`)

`LAZYOS_LAZYVM=1` (`run_demo.py --lazyvm`) copies LazyVM's staging tree, as
it is, to `fhs::LAZYVM` (`/system/lazyvm`): `bin/lazyvm` (the static VM,
executable like everything under the stage's `bin/`), the class library
(`lib/lazyrt.jar` or the class tree `lib/lazyrt/`) and `tests/` (classes or
jars). The stage is `target/lazyvm/stage`, or the directory
`LAZYOS_LAZYVM_STAGE` names; LazyVM's own tools write it. With the switch on,
a missing stage (or one without `bin/lazyvm`) fails the build with a message;
off, nothing is read. Links in the stage are followed; a broken link, a
name that is not UTF-8, or a directory reached twice (a link cycle, or two
links to one tree) also fails the build, naming the path. A top-level
`commands` file in the stage is replaced by `LAZYOS_LAZYVM_COMMANDS` when that
is set. Code: `build_support/lazyvm_embed.rs`, tests
`cargo test -p build-support-tests lazyvm`.

## Running LazyVM tests

`tools/lazyvm/run.py` runs command lines inside a real LazyOS boot under
QEMU and writes what each printed (LOS-01):

```bash
python tools/lazyvm/run.py --commands cmds.txt --out results [--no-build] [--accel none] [--timeout 120]
```

`cmds.txt` has one command per line, with LazyOS paths (`/system/lazyvm/bin/lazyvm
-Xbootclasspath:/system/lazyvm/lib/lazyrt -cp /system/lazyvm/tests s1.Hello`),
split like a POSIX shell (`shlex`; blank lines and `#` comments skipped; an
unbalanced quote is an error naming its line). The tool builds the `lazyvm`
ABI fixture (`tools/abi/fixtures/src/lazyvm.rs`) and an image whose boot is
that fixture, with the stage and the command list (`LAZYOS_LAZYVM_COMMANDS`,
at `fhs::LAZYVM_COMMANDS`) on a fresh 256 MiB OS volume, and copies it to
`target/lazyvm/lazyos-lazyvm.img`. The image build always writes
`target/lazyos.img`, so the tool moves a developer's image there to
`target/lazyvm/lazyos.img.saved` for the build and puts it back afterwards
(`--image` may not name either path). If it cannot put it back, it says where
the image is, and later runs refuse to start until that file is moved back. Only the stage, `LAZYOS_OS_SIZE` and
`LAZYOS_LIMIT_*` of the caller's `LAZYOS_*` variables reach that build.

The fixture runs the commands in order through the Linux ABI, stdin closed,
each in a process group of its own, and streams each one's stdout and stderr
hex-encoded over the serial log while it reads them, so the bytes arrive
exactly (no CR/LF translation; nothing is normalized). For command `N` (from
1) the tool writes `results/N.stdout` and `results/N.stderr` (separate, each
at most 8 MiB; the tool warns when a stream was cut) and `results/N.exit`, the
status in decimal:

| Status | Meaning |
|--------|---------|
| the program's own | it exited |
| 128 + n | signal n ended it |
| 124 | `--timeout` (per command, default 120 s, at least 1) ran out; the group was killed |
| 125 | no exit status came back (a failed `wait`, or none 5 s after the kill) |
| 127 | it could not start |

The group is also killed when the pipes stay open 5 s after the program
exited (a grandchild holding them). Result files of an earlier run in
`results/` are removed first. The host waits on progress, not a fixed
allowance: the boot has 300 s to reach the fixture, a command its timeout
plus the fixture's grace, and the run fails after 60 s with no marker line
beyond that. It exits 0 when every command ran, whatever their statuses, and
1 with a message when the build, the boot or the harness failed (serial log in
`results/qemu/serial.log`). The command list is part of the image, so
`--no-build` boots the last image as it is and fails if it was built for
other commands. Self-test: `python tools/lazyvm/test_judge.py`.

Work in this repo for LazyVM follows this repo's `AGENTS.md` (build switch,
core package, both front ends, evidence by pixels and serial markers) **and**
the shared LazyVM rules below. The block is an identical copy of the one in
LazyVM's `AGENTS.md`, which is the master; LazyVM's `tools/check_rules.py
--lazyos <path>` compares them.

<!-- lazyvm-rules:begin -->
## Shared rules for LazyVM work

**1. What you may read and copy (clean room).**
- You may read: the JVMS SE 8, the JLS SE 8 and the Java SE 8 API Javadoc
  (docs.oracle.com/javase/specs and docs.oracle.com/javase/8/docs/api); class
  files you compiled yourself, and `javap` output of them; the observable
  behaviour of a JVM running our tests (stdout, exceptions, timing); this repo;
  LazyOS; xui; and the documentation of crates we depend on.
- You must not open, read, search, decompile or copy: OpenJDK or any other
  JDK's source (`src.zip`, github.com/openjdk, mirrors, code search results),
  decompiled or disassembled JDK classes (`rt.jar`, `jmods`), or the source of
  another class library or JVM (GNU Classpath, Apache Harmony, Android
  libcore, J9, GraalVM). This applies to snippets in web results and to code
  "from memory". Write it from the spec instead. If a spec is unclear, write
  a test, bless it on the oracle JVM and match its output.
- Do not paste spec or Javadoc prose into the repo. Write doc comments in your
  own words, and cite the section (`// JVMS 5.4.3.3`) instead.

**2. Namespace and naming.**
- Add no `public` or `protected` type, field or method to `java.*` or
  `javax.*` beyond what the Java SE 8 API declares, with its exact signature.
  Helpers are package-private or go in `lazy.internal.*`. Our API is `lazy.*`.
- Never call the product "Java" or "Java compatible", and never use Oracle
  marks in names. It is "LazyVM".

**3. What counts as done (evidence over assertion).**
- A work item is done only when its **Check** in `docs/plan.md` (or `plan-2.md`) passes,
  run by you, on every platform it names. Do not claim behaviour from reading
  the source or from a build that merely compiles.
- `java.*` behaviour is judged by differential tests: `tests/java/...` goldens
  blessed on the oracle JVM with `tools/golden.py --bless`. Never hand-edit a
  `java.*` golden to make a test pass. `lazy.*` goldens are hand-written and
  reviewed.
- From S3 onward, "both" means Windows and LazyOS (`tools/lazyos_test.py`). UI
  work is judged by pixels (goldens or screenshots), not by log lines.
- Say plainly what you could not check, for example an IntelliJ step that
  needs the owner.

**4. Small, clean, one way.**
- Files stay under 500 lines; functions are small and single-purpose;
  comments say why. There is one way to do each thing: reuse the existing
  helper, and do not add a second.
- Every `unsafe` block is minimal, lives in `crates/vm/src/heap` or
  `crates/vm/src/interp` unless the plan says otherwise, and has a
  `// SAFETY:` comment.
- Malformed class files, jars and network input are errors, never panics.
- New dependencies must have a permissive license (MIT, Apache-2.0, BSD, Zlib,
  ISC), be recorded in `THIRD_PARTY.md`, and pass `python tools/licenses.py`.
  Never add `ring`, `aws-lc` or a GPL crate outside `crates/lazyvm-lazyos`.

**5. Review and merge.**
- Before every push, run the CI basics (`AGENTS.md`, "Before you push"). A
  failing gate is fixed at its cause, never skipped, ignored or loosened.
- Before opening a PR, or marking one ready, review the committed branch in
  a fresh context: the `/code-review` skill (`high` effort for the VM core,
  the class-file parser, the verifier, natives and anything parsing untrusted
  input), or a separate reviewer agent given the diff and not your
  conclusions. Fix every real finding, re-run until a pass finds nothing
  real, and note in the PR which review ran and what changed.
- A PR may be merged (auto-merge included) only when the full CI suite is
  green.
<!-- lazyvm-rules:end -->
